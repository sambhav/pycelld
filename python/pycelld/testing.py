"""Real-process HTTP fixtures shared by pytest and repository checks."""
from __future__ import annotations

from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
from typing import Callable, Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

from .check import check
from .install import resolve_binary


@dataclass
class Response:
    status: int
    headers: dict[str, str]
    body: bytes

    @property
    def text(self) -> str:
        return self.body.decode()

    def json(self) -> Any:
        return json.loads(self.body)


def wait_until(predicate: Callable[[], Any], *, timeout: float = 15, interval: float = 0.05) -> Any:
    """Wait for a real alarm/event, without replacing celld's clock."""
    deadline = time.monotonic() + timeout
    while True:
        result = predicate()
        if result:
            return result
        if time.monotonic() >= deadline:
            raise TimeoutError(f"condition did not become true within {timeout}s")
        time.sleep(min(interval, max(0, deadline - time.monotonic())))


def stop_process(process: subprocess.Popen) -> None:
    """Stop a supervisor created with start_new_session=True and its children."""
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


class Worker:
    """An isolated copy of a project served by the actual celld executable.

    Restart preserves this copy's durable state. Closing deletes only the
    fixture's own temporary directory. CELLD_* settings must be passed explicitly
    so a developer's production bucket/policy configuration cannot leak in.
    """

    def __init__(self, project: str | Path, *, binary: str | Path | None = None,
                 env: dict[str, str] | None = None, timeout: float = 120):
        self.binary = resolve_binary(binary)
        self._directory = tempfile.TemporaryDirectory(prefix="pycelld-test-")
        self.root = Path(self._directory.name)
        self.project = self.root / "app"
        try:
            shutil.copytree(project, self.project, ignore=shutil.ignore_patterns(".celld", ".git", ".venv", "__pycache__", ".pytest_cache"))
        except BaseException:
            self._directory.cleanup()
            raise
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("CELLD_")}
        self.env.update(CELLD_MAX_STATELESS_ISOLATES="2", NO_COLOR="1")
        self.env.update(env or {})
        self.timeout = timeout
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        self.url = f"http://127.0.0.1:{self.port}"
        self.log_path = self.root / "server.log"
        self.process: subprocess.Popen | None = None
        self._log = None

    @property
    def logs(self) -> str:
        return self.log_path.read_text(errors="replace") if self.log_path.exists() else ""

    def start(self) -> Worker:
        if self.process is not None:
            raise RuntimeError("worker is already running")
        check(self.project, self.binary, env=self.env)
        self._log = self.log_path.open("a")
        try:
            self.process = subprocess.Popen(
                [self.binary, "dev", str(self.project), "--port", str(self.port), "--logs"],
                env=self.env, stdout=self._log, stderr=self._log, start_new_session=True,
            )
            def ready() -> bool:
                if self.process.poll() is not None:
                    raise RuntimeError("celld exited during startup:\n" + self.logs)
                try:
                    # Public handlers may answer while the supervisor is still
                    # waiting on readiness. Require its health gate before tests
                    # run, and never invoke a user's catchall handler as a probe.
                    return self.request("/.well-known/celld/health", timeout=0.5).status == 200
                except (OSError, URLError, TimeoutError):
                    return False
            wait_until(ready, timeout=self.timeout)
        except BaseException:
            self.stop()
            raise
        return self

    def stop(self) -> None:
        process, self.process = self.process, None
        try:
            if process is not None:
                stop_process(process)
        finally:
            if self._log:
                self._log.close()
                self._log = None

    def restart(self) -> Worker:
        self.stop()
        return self.start()

    def close(self) -> None:
        self.stop()
        self._directory.cleanup()

    def __enter__(self) -> Worker:
        try:
            return self.start()
        except BaseException:
            self.close()
            raise

    def __exit__(self, *_: object) -> None:
        self.close()

    def request(self, path: str, *, method: str = "GET", body: bytes | None = None,
                headers: dict[str, str] | None = None, timeout: float = 30) -> Response:
        request = Request(self.url + "/" + path.lstrip("/"), data=body, method=method, headers=headers or {})
        try:
            response = urlopen(request, timeout=timeout)
        except HTTPError as error:
            response = error
        with response:
            return Response(response.status, {k.lower(): v for k, v in response.headers.items()}, response.read())

    def call(self, name: str, arguments: dict | None = None) -> Any:
        response = self.request(name, method="POST", body=json.dumps(arguments or {}).encode(), headers={"Content-Type": "application/json"})
        if response.status >= 400:
            raise AssertionError(f"{name}: HTTP {response.status}: {response.text}\n{self.logs}")
        if response.status == 204:
            return None
        if "application/json" in response.headers.get("content-type", ""):
            return response.json()
        return response.text

    wait_until = staticmethod(wait_until)


@dataclass
class FetchRequest:
    method: str
    path: str
    headers: dict[str, str]
    body: bytes


class FetchServer:
    """Loopback upstream fixture. This never grants workers network permission.

    Pass an explicit host policy through Worker(env=...) when testing allowed
    traffic. Requests are recorded so a deny test can assert no transport ran.
    """

    def __init__(self):
        self.requests: list[FetchRequest] = []
        self._routes: dict[str, Response] = {}
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                size = int(self.headers.get("Content-Length", "0"))
                if size < 0 or size > 1024 * 1024:
                    self.send_error(413)
                    return
                fixture.requests.append(FetchRequest(self.command, self.path, dict(self.headers.items()), self.rfile.read(size)))
                response = fixture._routes.get(self.path, Response(404, {}, b"no fixture for this path"))
                self.send_response(response.status)
                for key, value in response.headers.items():
                    self.send_header(key, value)
                self.send_header("Content-Length", str(len(response.body)))
                self.end_headers()
                self.wfile.write(response.body)

            do_POST = do_PUT = do_PATCH = do_DELETE = do_GET

            def log_message(self, *_):
                pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self._server.server_port}"
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def respond(self, path: str, body: bytes | str | dict | list, *, status: int = 200,
                headers: dict[str, str] | None = None) -> None:
        headers = dict(headers or {})
        if isinstance(body, (dict, list)):
            body = json.dumps(body).encode()
            headers.setdefault("Content-Type", "application/json")
        elif isinstance(body, str):
            body = body.encode()
        self._routes[path] = Response(status, headers, body)

    def close(self) -> None:
        self._server.shutdown()
        self._server.server_close()
        self._thread.join(timeout=5)

    def __enter__(self) -> FetchServer:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()
