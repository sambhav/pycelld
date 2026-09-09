"""Exercise explicit Python HTTP handlers against the real celld dev server."""
import http.client
import json
import os
from pathlib import Path
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
    shutil.copytree("examples/http", project)
    source = project / "worker.py"
    source.write_text('''from celld import http, Response
@http
async def handle(request, ctx):
    if request.path == '/binary':
        return Response(request.body, status=201, headers=[('Set-Cookie', 'a=1'), ('Set-Cookie', 'b=2')])
    if request.path == '/json':
        try: return request.json()
        except ValueError: return Response('invalid JSON', status=400)
    if request.path == '/large': return b'x' * (1024 * 1024 + 1)
    if request.path == '/fail': raise ValueError('HTTP handler failed')
    if request.path == '/slow':
        await ctx.sleep(5)
        return 'complete'
    return {'method':request.method, 'path':request.path, 'text':request.text(),
            'query':request.query, 'tags':request.get_all_query('tag'),
            'headers':request.get_all_headers('X-Test')}
def hello(name: str = 'world'): return name
''')
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = {k: v for k, v in os.environ.items() if not k.startswith("CELLD_")}
    env["CELLD_MAX_STATELESS_ISOLATES"] = "1"
    log_path = root / "server.log"

    def request(method, path, body=b"", headers=()):
        client = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        client.putrequest(method, path)
        for name, value in headers:
            client.putheader(name, value)
        client.putheader("Content-Length", str(len(body)))
        client.endheaders(body)
        response = client.getresponse()
        result = (response.status, response.getheaders(), response.read())
        client.close()
        return result

    with log_path.open("w") as log:
        process = subprocess.Popen([binary, "dev", str(project), "--port", str(port)],
                                   env=env, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 120
            while True:
                assert process.poll() is None, log_path.read_text()
                try:
                    request("GET", "/")
                    break
                except (OSError, http.client.HTTPException):
                    assert time.monotonic() < deadline, log_path.read_text()
                    time.sleep(.1)
            for method in ["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"]:
                status, _, body = request(method, "/v1/items?tag=one&tag=two&name=a+b", b"hello",
                                          [("X-Test", "a"), ("X-Test", "b")])
                assert status == 200
                assert json.loads(body) == {"method": method, "path": "/v1/items", "text": "hello",
                                           "query": {"tag": "two", "name": "a b"}, "tags": ["one", "two"],
                                           "headers": ["a", "b"]}
            status, headers, body = request("PUT", "/binary", b"\x00\xff\x80")
            assert (status, body) == (201, b"\x00\xff\x80")
            assert [v for k, v in headers if k.lower() == "set-cookie"] == ["a=1", "b=2"]
            assert request("POST", "/json", b"{")[0] == 400
            assert request("POST", "/json", b"[1,2]")[2] == b"[1,2]"
            assert request("POST", "/hello", b"{}")[2] == b"world"
            assert request("GET", "/hello")[0] == 405
            assert json.loads(request("POST", "/handle", b"raw")[2])["text"] == "raw"
            assert request("HEAD", "/v1/items")[2] == b""
            assert json.loads(request("GET", "/v1/%FF")[2])["path"] == "/v1/%FF"
            assert request("PUT", "/binary", b"x" * (1024 * 1024 + 1))[0] == 413
            assert request("GET", "/large")[0] == 500
            status, _, body = request("GET", "/fail")
            assert status == 500 and json.loads(body)["error"]["code"] == "ValueError"
            # More simultaneous abandoned awaits than the native capacity. Each
            # disconnected request must drop its continuation and timer.
            for _ in range(260):
                with socket.create_connection(("127.0.0.1", port), timeout=10) as client:
                    client.sendall(b"GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    time.sleep(.003)
            time.sleep(.2)
            assert request("POST", "/hello", b"{}")[2] == b"world"
            print("Explicit HTTP routing, binary bodies, repeated headers, limits and cancellation passed.")
        except BaseException:
            print(log_path.read_text(), file=sys.stderr)
            raise
        finally:
            process.terminate()
            try: process.wait(timeout=15)
            except subprocess.TimeoutExpired: process.kill(); process.wait()
