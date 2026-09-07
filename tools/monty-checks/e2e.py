"""Real HTTP, native storage, object RPC, buffered responses, alarms and disk recovery."""
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from urllib.request import Request, urlopen
from urllib.error import HTTPError, URLError
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    project = root / "app"
    shutil.copytree("examples/monty", project)
    source = project / "worker.py"
    text = source.read_text().replace("    def schedule(self,", '''    def inspect(self):
        return {"id": self.id, "count": self._ctx.storage.get("count", 0),
                "alarmed": self._ctx.storage.get("alarmed", False)}

    def rollback(self):
        try:
            with self._ctx.storage.transaction() as tx:
                tx.set("count", -999)
                with tx.transaction() as nested:
                    nested.set("nested", True)
                raise ValueError("rollback")
        except ValueError:
            return self.inspect()

    def sql(self):
        storage = self._ctx.storage
        storage.sql("CREATE TABLE IF NOT EXISTS test (id INTEGER PRIMARY KEY, value TEXT)")
        storage.sql("INSERT OR REPLACE INTO test VALUES (?, ?)", 1, "works")
        return storage.sql("SELECT value FROM test WHERE id = ?", 1)

    def disallow_io(self):
        try:
            with self._ctx.storage.transaction() as tx:
                tx.set("count", -100)
                return Counter("other", self._ctx).increment()
        except RuntimeError:
            return self.inspect()

    async def wait_increment(self, seconds: float = 0.01):
        value = self._ctx.storage.get("async", 0)
        await self._ctx.sleep(seconds)
        self._ctx.storage.set("async", value + 1)
        return value + 1

    def async_count(self): return self._ctx.storage.get("async", 0)
    def metadata(self): return {"url": self._ctx.request.url, "env": self._ctx.env, "id": self._ctx.id}
    def fail(self): raise ValueError("remote failure")
    def sync(self):
        self._ctx.storage.set("synced", True)
        self._ctx.storage.sync()
        return self._ctx.storage.get("synced")

    def schedule(self,''')
    text += '''
def inspect(ctx: Context, id: str): return Counter(id, ctx).inspect()
def rollback(ctx: Context, id: str): return Counter(id, ctx).rollback()
def sql(ctx: Context, id: str): return Counter(id, ctx).sql()
def schedule(ctx: Context, id: str): return Counter(id, ctx).schedule(1)
def disallow_io(ctx: Context, id: str): return Counter(id, ctx).disallow_io()
def metadata(ctx: Context): return {"url":ctx.request.url, "env":ctx.env, "id":ctx.id}
def binary(): return b"abc\\x00\\xff"
def _private(): return "secret"
async def slow(ctx: Context):
    await ctx.sleep(0.2)
    return {"awake": True}
def iterator(): return iter(range(3))
def echo(value): return value
def custom(): return Response('created', status=201, headers={"X-Test":"yes"})
def invalid_header(): return Response('bad', headers={"x-test":"bad\\nvalue"})
def now(ctx): return ctx.now()
async def upstream_post(ctx: Context, url: str):
    return await ctx.fetch(url, method="POST", body=b'{"value":{"ok":true}}')
'''
    text += """
async def wait_increment(ctx: Context, id: str, seconds: float = 0.01): return await Counter(id, ctx).wait_increment(seconds)
def async_count(ctx: Context, id: str): return Counter(id, ctx).async_count()
def object_metadata(ctx: Context, id: str): return Counter(id, ctx).metadata()
def sync(ctx: Context, id: str): return Counter(id, ctx).sync()
def remote_error(ctx: Context, id: str):
    try: return Counter(id, ctx).fail()
    except ValueError as error: return str(error)
def big_binary(): return b'\\xff' * 400000
async def fetched_bytes(ctx: Context, url: str):
    response = await ctx.fetch(url, method='POST', body='{}')
    return len(response.body)
"""
    def write_source(text):
        source.write_text(text)
    write_source(text)
    config = {"name":"monty-e2e", "main":"worker.py", "vars":{"GREETING":"hi"}}
    (project / "wrangler.jsonc").write_text(json.dumps(config))
    with socket.socket() as sock:
        sock.bind(("127.0.0.1",0))
        port = sock.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    env = {k:v for k,v in os.environ.items() if not k.startswith("CELLD_")}
    env.update(CELLD_MAX_STATELESS_ISOLATES="2")
    log_path = root / "server.log"
    def request(name, args=None):
        return Request(url+"/"+name, data=json.dumps(args or {}).encode(), headers={"content-type":"application/json"})
    def call(name, args=None, *, during_reload=False):
        try:
            with urlopen(request(name,args),timeout=30) as response:
                body = response.read()
                if response.status == 204: return None
                if "application/json" in response.headers.get("content-type", ""): return json.loads(body)
                return body.decode()
        except HTTPError as error:
            body = error.read().decode()
            if during_reload and error.code == 503 and json.loads(body) == {"ok": False, "draining": True}:
                return None
            raise AssertionError(f"{name}: {error.code}: {body}") from error
    def start(log):
        process = subprocess.Popen([binary,"dev",str(project),"--port",str(port),"--logs"],env=env,stdout=log,stderr=log)
        deadline = time.monotonic()+120
        while time.monotonic()<deadline:
            if process.poll() is not None: raise AssertionError(log_path.read_text())
            try:
                with socket.create_connection(("127.0.0.1",port),timeout=.2): return process
            except OSError: time.sleep(.1)
        process.kill(); process.wait(); raise AssertionError(log_path.read_text())
    def stop(process):
        process.terminate()
        try: process.wait(timeout=15)
        except subprocess.TimeoutExpired: process.kill(); process.wait()
    process = None
    with log_path.open("w") as log:
        try:
            process = start(log)
            assert call("hello") == "Hello, world!"
            # Chunked input is bounded and collected by the native driver.
            import http.client
            client = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            client.request("POST", "/echo", body=iter([b'{"value":', b'42}']), headers={"content-type":"application/json"}, encode_chunked=True)
            response = client.getresponse()
            assert response.status == 200 and response.read() == b"42"
            client.close()
            for path, method, body, status in [
                ("/hello", "GET", b"", 405), ("/missing", "POST", b"{}", 404),
                ("/hello", "POST", b"{", 400), ("/hello", "POST", b"[]", 400),
                ("/hello", "POST", b"\xff", 400),
                ("/hello", "POST", b"x"*(1024*1024+1), 413),
            ]:
                client = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
                client.request(method, path, body=body)
                response = client.getresponse()
                assert response.status == status, (path, method, response.status, response.read())
                response.read(); client.close()

            assert call("slow") == {"awake":True}
            assert call("upstream_post", {"url":url+"/echo"}) == {"ok":True}
            assert call("metadata") == {"url":url+"/metadata","env":{"GREETING":"hi"},"id":None}
            key = "cart/東京"
            assert call("increment", {"id":key}) == {"id":key,"value":1}
            with ThreadPoolExecutor(max_workers=8) as pool:
                values = list(pool.map(lambda _:call("increment",{"id":key})["value"],range(24)))
            assert sorted(values) == list(range(2,26)), values
            assert call("increment",{"id":"other","amount":10})["value"] == 10
            assert call("rollback",{"id":key})["count"] == 25
            assert call("disallow_io",{"id":key})["count"] == 25
            assert call("inspect",{"id":"other"})["count"] == 10
            assert call("sql",{"id":key}) == [{"value":"works"}]
            call("schedule",{"id":key})
            deadline = time.monotonic()+15
            while not call("inspect",{"id":key})["alarmed"]:
                assert time.monotonic()<deadline, "alarm did not fire"
                time.sleep(.1)
            assert call("sync", {"id": key}) is True
            assert call("remote_error", {"id": key}) == "remote failure"
            assert call("object_metadata", {"id":key}) == {"url":url+"/object_metadata", "env":{"GREETING":"hi"}, "id":key}
            assert call("fetched_bytes", {"url":url+"/big_binary"}) == 400000
            with ThreadPoolExecutor(max_workers=8) as pool:
                values = list(pool.map(lambda _:call("wait_increment",{"id":key}), range(16)))
            assert sorted(values) == list(range(1,17)), values
            # Disconnect while the durable method owns its input gate. The
            # nested request must be cancelled before it can commit its write.
            with socket.create_connection(("127.0.0.1",port),timeout=10) as client:
                body = json.dumps({"id":"cancelled", "seconds":5}).encode()
                client.sendall(f"POST /wait_increment HTTP/1.1\r\nHost: localhost\r\nContent-Length: {len(body)}\r\n\r\n".encode()+body)
                time.sleep(.2)
            started = time.monotonic()
            assert call("async_count", {"id":"cancelled"}) == 0
            assert time.monotonic() - started < 3, "cancelled durable method retained its input gate"
            assert call("now").endswith("+00:00")
            large = "x" * 70000
            assert call("echo", {"value":large}) == large
            number = 340282366920938463463374607431768211457
            assert call("echo", {"value":{"n":number}}) == {"n":number}
            with urlopen(request("custom"),timeout=10) as response:
                assert response.status == 201 and response.headers["x-test"] == "yes"
                assert response.headers["content-type"] == "text/plain;charset=UTF-8"
                assert response.read() == b"created"
            with urlopen(request("binary"),timeout=10) as response:
                assert response.read() == b"abc\x00\xff"
            for name,args,status in [("hello",{"ctx":{}},400),("hello",{"name":1},400),("_private",{},404),("fail",{},500),("iterator",{},500),("invalid_header",{},500)]:
                try: urlopen(request(name,args),timeout=10)
                except HTTPError as error:
                    assert error.code == status, (name,error.code,error.read())
                else: raise AssertionError("invalid call accepted: "+name)
            # Cancel asynchronous sessions before they finish, then prove the
            # request capacity remains available. Client hangs release the native
            # continuation and its timer future.
            for _ in range(260):
                with socket.create_connection(("127.0.0.1",port),timeout=10) as client:
                    client.sendall(b"POST /slow HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}")
                    time.sleep(.002)
            time.sleep(.3)
            assert call("hello") == "Hello, world!"
            write_source(text.replace("Hello, {name}!", "Welcome, {name}!"))
            deadline=time.monotonic()+45
            while True:
                try:
                    if call("hello", during_reload=True) == "Welcome, world!": break
                except (URLError, ConnectionError, http.client.HTTPException):
                    pass  # Dev reload briefly closes and reopens the listener.
                assert process.poll() is None, "dev process exited during reload"
                assert time.monotonic()<deadline,"reload did not apply"
                time.sleep(.2)
            assert call("inspect",{"id":key})["count"] == 25
            stop(process); process=None
            process=start(log)
            assert call("inspect",{"id":key})["count"] == 25
            print("Native Monty HTTP, async cancellation, direct objects, transactions, SQL, alarms, reload and restart passed.")
        except BaseException:
            print(log_path.read_text(),file=sys.stderr)
            raise
        finally:
            if process is not None: stop(process)
