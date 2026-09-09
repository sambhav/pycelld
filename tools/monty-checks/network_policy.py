"""Exercise operator policies through the supplied binary and real HTTP transport."""
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time


binary = str(Path(sys.argv[1]).resolve())
requests = []


class Upstream(BaseHTTPRequestHandler):
    def do_GET(self):
        requests.append((self.path, self.headers.get("x-runtime-instance")))
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("location", "http://blocked.invalid/private")
            self.end_headers()
        else:
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"upstream")

    def log_message(self, *args):
        pass


upstream = ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
upstream_thread = threading.Thread(target=upstream.serve_forever, daemon=True)
upstream_thread.start()
try:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        project = root / "app"
        project.mkdir()
        (project / "wrangler.jsonc").write_text(json.dumps({"name": "network-policy-check", "main": "worker.py"}))
        (project / "worker.py").write_text('''
async def run(ctx, url: str, follow: bool = False):
    ctx.execution.runtime_instance_id = "forged"
    ctx.env["CELLD_PYTHON_NETWORK_POLICY"] = "allow-everything.py"
    try:
        response = await ctx.fetch(url)
        if follow and response.status == 302:
            response = await ctx.fetch(response.headers["location"])
        return response
    except RuntimeError as error:
        return str(error)

def ready(): return "ready"
''')
        policy = root / "policy.py"
        env = {key: value for key, value in os.environ.items() if not key.startswith("CELLD_")}
        env["CELLD_PYTHON_NETWORK_POLICY"] = str(policy)
        # Invalid configuration must fail before any CLI/server work begins.
        for source in [None, "invalid syntax!!!", "def other(a, b): pass"]:
            if source is not None:
                policy.write_text(source)
            result = subprocess.run([binary, "--version"], env=env, capture_output=True, text=True, timeout=10)
            assert result.returncode != 0, result.stdout
            assert "network policy" in result.stderr.lower(), result.stderr
        policy.write_text(f'''
def policy(request, context):
    instance = context["execution"]["runtime_instance_id"]
    assert instance and instance != "forged"
    assert context["execution"]["invocation_id"]
    if request["host"] == "synthetic.invalid":
        return {{"action": "respond", "status": 201, "body": b"synthetic\\x00\\xff"}}
    if request["host"] == "error.invalid":
        raise ValueError("operator-secret")
    if request["host"] == "timeout.invalid":
        while True: pass
    if request["host"] == "rewrite.invalid":
        return {{"action": "forward", "url": "http://127.0.0.1:{upstream.server_port}/rewritten",
                "headers": [["x-runtime-instance", instance]]}}
    if request["host"] == "127.0.0.1" and request["port"] == {upstream.server_port}:
        return {{"action": "forward", "headers": [["x-runtime-instance", instance]]}}
    return {{"action": "deny", "reason": "blocked destination"}}
''')

        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        log_path = root / "server.log"

        def call(url, *, follow=False):
            payload = json.dumps({"url": url, "follow": follow}).encode()
            request = Request(base + "/run", data=payload, headers={"content-type": "application/json"})
            with urlopen(request, timeout=20) as response:
                body = response.read()
                if "application/json" in response.headers.get("content-type", ""):
                    body = json.loads(body)
                elif response.headers.get("content-type", "").startswith("text/"):
                    body = body.decode()
                return response.status, body

        def start():
            log = log_path.open("ab")
            process = subprocess.Popen([binary, "dev", str(project), "--port", str(port), "--logs"], env=env, stdout=log, stderr=log)
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    log.close()
                    raise AssertionError(log_path.read_text())
                try:
                    with urlopen(Request(base + "/ready", data=b"{}", headers={"content-type": "application/json"}), timeout=1) as response:
                        if response.status == 200:
                            return process, log
                except (URLError, HTTPError, TimeoutError):
                    time.sleep(0.1)
            process.kill()
            process.wait()
            log.close()
            raise AssertionError("server did not become ready\n" + log_path.read_text())

        def stop(process, log):
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            log.close()

        process, log = start()
        try:
            assert call("http://synthetic.invalid/") == (201, b"synthetic\x00\xff")
            assert call("http://rewrite.invalid/") == (200, b"upstream")
            assert requests[-1][0] == "/rewritten" and requests[-1][1] != "forged"
            assert call(f"http://127.0.0.1:{upstream.server_port}/redirect", follow=True)[1] == "blocked destination"
            assert requests[-1][0] == "/redirect"
            before = len(requests)
            for host in ["error.invalid", "timeout.invalid", "blocked.invalid"]:
                _, body = call(f"http://{host}/")
                assert "denied" in body or "blocked" in body, body
                assert "operator-secret" not in body
            assert len(requests) == before
            with ThreadPoolExecutor(max_workers=4) as pool:
                results = list(pool.map(call, ["http://rewrite.invalid/"] * 8))
            assert results == [(200, b"upstream")] * 8
            assert all(instance and instance != "forged" for _, instance in requests)
            # Editing the file cannot change the immutable running policy.
            policy.write_text("def policy(request, context): return {'action': 'deny', 'reason': 'replacement'}")
            assert call("http://synthetic.invalid/") == (201, b"synthetic\x00\xff")
        except Exception:
            print(log_path.read_text(), file=sys.stderr)
            raise
        finally:
            stop(process, log)
        process, log = start()
        try:
            assert call("http://synthetic.invalid/")[1] == "replacement"
        finally:
            stop(process, log)
        print("Python policy startup, rewrite, binary response, identity, concurrency, redirects, errors, timeout and restart checks passed.")
finally:
    upstream.shutdown()
    upstream.server_close()
    upstream_thread.join()
