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

# The public pytest fixture and this broader protocol suite share one real
# process lifecycle, including isolated copies, restart and cleanup.
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "python"))
from pycelld.testing import Worker

binary = str(Path(sys.argv[1]).resolve())
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    project = root / "app"
    shutil.copytree("examples/monty", project)
    source = project / "worker.py"
    text = ("from celld import Response\n" + source.read_text()).replace("    def schedule(self,", '''    def inspect(self):
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
    try:
        await ctx.fetch(url, method="POST", body=b'{"value":{"ok":true}}')
    except RuntimeError as error:
        return str(error)
    return "unexpected network access"
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
    try:
        await ctx.fetch(url, method='POST', body='{}')
    except RuntimeError as error:
        return str(error)
    return "unexpected network access"
"""
    text = "from pathlib import Path\n" + text
    text = text.replace("    def inspect(self):", """    def files(self, action: str):
        p = Path('notes/latest.txt')
        if action == 'save':
            p.parent.mkdir(parents=True, exist_ok=True)
            with open(str(p), 'w') as f:
                f.write('hello')
                f.write(' durable')
            with open(str(p), 'a') as f:
                f.write(' files')
            Path('notes/raw').write_bytes(b'\\x00\\xff')
            self._ctx.storage.sync()
        if action == 'rollback':
            try:
                with self._ctx.storage.transaction() as tx:
                    tx.set('file_transaction', 'changed')
                    p.write_text('rolled back')
                    Path('notes/raw').rename('notes/moved')
                    raise ValueError('rollback')
            except ValueError:
                pass
        if action == 'missing':
            try:
                p.read_text()
            except FileNotFoundError:
                return 'missing'
        if action == 'clear':
            self._ctx.storage.clear()
            return p.exists()
        return {'text': p.read_text(), 'size': p.stat().st_size,
                'raw': Path('notes/raw').read_bytes() == b'\\x00\\xff',
                'files': sorted([str(x) for x in Path('notes').iterdir()]),
                'rolled_back': self._ctx.storage.get('file_transaction') is None}

    def inspect(self):""")
    text += """
def files(ctx: Context, id: str, action: str): return Counter(id, ctx).files(action)
def stateless_files():
    try:
        Path('anything').write_text('blocked')
    except PermissionError:
        return 'denied'
"""
    def write_source(text):
        source.write_text(text)
    write_source(text)
    config = {"name":"monty-e2e", "main":"worker.py", "vars":{"GREETING":"hi"}}
    (project / "wrangler.jsonc").write_text(json.dumps(config))
    worker = Worker(project, binary=binary)
    project = worker.project
    source = project / "worker.py"
    port = worker.port
    url = worker.url
    log_path = worker.log_path
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
        worker.start()
        return worker.process
    def stop(process):
        worker.stop()
    process = None
    with log_path.open("w") as log:
        try:
            process = start(log)
            assert call("hello") == "Hello, world!"
            expected_files = {'text': 'hello durable files', 'size': 19, 'raw': True,
                              'files': ['/notes/latest.txt', '/notes/raw'], 'rolled_back': True}
            assert call('stateless_files') == 'denied'
            assert call('files', {'id':'files', 'action':'save'}) == expected_files
            assert call('files', {'id':'files', 'action':'rollback'}) == expected_files
            assert call('files', {'id':'other-files', 'action':'missing'}) == 'missing'
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
            assert "outbound HTTP is disabled" in call("upstream_post", {"url":url+"/echo"})
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
            assert "outbound HTTP is disabled" in call("fetched_bytes", {"url":url+"/big_binary"})
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
            assert call('files', {'id':'files', 'action':'read'}) == expected_files
            assert call('files', {'id':'files', 'action':'clear'}) is False
            print("Native Monty filesystem, HTTP, async cancellation, direct objects, transactions, SQL, alarms, reload and restart passed.")
        except BaseException:
            print(log_path.read_text(),file=sys.stderr)
            raise
        finally:
            worker.close()
