"""Run with PYCELLD_BINARY or --celld-binary; CI requires the real executable."""
import os
import gzip
import hashlib
import json
import re
import socket
from pathlib import Path
import subprocess
import sys
from urllib.request import Request, urlopen
from urllib.error import URLError

import pytest

from pycelld.check import CheckError, check
from pycelld.cli import init
from pycelld.install import resolve_binary
from pycelld.testing import Worker, stop_process, wait_until
from pycelld import install as installer


@pytest.fixture
def binary(request):
    supplied = request.config.getoption("--celld-binary") or os.environ.get("PYCELLD_BINARY")
    if not supplied:
        pytest.skip("set PYCELLD_BINARY to run the real-binary integration checks")
    return resolve_binary(supplied)


def test_init_check_dev_pytest_flow(binary, tmp_path):
    project = tmp_path / "hello"
    subprocess.run([sys.executable, "-m", "pycelld", "--binary", binary, "init", project], check=True)
    generated = subprocess.check_output([binary, "types"], text=True)
    assert (project / "celld.pyi").read_text() == generated
    subprocess.run([sys.executable, "-m", "pycelld", "--binary", binary, "check", project], check=True)
    # The generated test exercises hello + durable restart and isolated state.
    result = subprocess.run([sys.executable, "-m", "pytest", "-q", "--celld-binary", binary], cwd=project, capture_output=True, text=True)
    assert result.returncode == 0, result.stdout + result.stderr
    assert not (project / ".celld").exists(), "pytest must not write app development state"


def test_install_actual_release_binary_and_generate_matching_project(binary, tmp_path, monkeypatch):
    version = subprocess.check_output([binary, "--version"], text=True).strip().removeprefix("celld ")
    if not re.fullmatch(r"\d+\.\d+\.\d+-pycelld\.\d+", version):
        pytest.skip("release installer integration runs against versioned binaries in the four-platform release matrix")
    target = installer.platform_target()
    data = binary.read_bytes()
    archive = gzip.compress(data, compresslevel=1)
    name = f"celld-{target}.gz"
    build = {"target": target, "version": version, "repository": installer.REPOSITORY, "archive": name,
             "sha256": hashlib.sha256(archive).hexdigest(), "binary_sha256": hashlib.sha256(data).hexdigest()}
    assets = {"latest": json.dumps({"tag_name": "v" + version}).encode(),
              "BUILD_INFO.json": json.dumps([build]).encode(), name: archive}
    # CI has not published its release yet. Feed the genuine built binary through
    # the same downloader boundary, then execute the installed result unchanged.
    monkeypatch.setattr(installer, "download", lambda url: assets[url.rsplit("/", 1)[1]])
    monkeypatch.setenv("PYCELLD_CACHE_DIR", str(tmp_path / "cache"))
    installed = installer.install()
    project = tmp_path / "installed-project"
    init(project, installed)
    assert (project / "celld.pyi").read_bytes() == (installed.parent / "types/celld.pyi").read_bytes()
    with Worker(project, binary=installed) as worker:
        assert worker.call("hello") == "Hello, world!"
        assert worker.call("increment") == 1
        worker.restart()
        assert worker.call("increment") == 2


def test_cli_dev_checks_and_serves_project(binary, tmp_path):
    project = tmp_path / "cli"
    init(project, binary)
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    env = {key: value for key, value in os.environ.items() if not key.startswith("CELLD_")}
    log_path = tmp_path / "cli.log"
    with log_path.open("w") as log:
        process = subprocess.Popen([sys.executable, "-m", "pycelld", "--binary", binary, "dev", project, "--port", str(port)],
                                   stdout=log, stderr=log, env=env, start_new_session=True)
        try:
            def ready():
                assert process.poll() is None, log_path.read_text()
                try:
                    with urlopen(f"http://127.0.0.1:{port}/.well-known/celld/health", timeout=0.5) as health:
                        if health.status != 200:
                            return False
                    with urlopen(Request(f"http://127.0.0.1:{port}/hello", data=b"{}"), timeout=0.5) as response:
                        return response.read() == b"Hello, world!"
                except (OSError, URLError, TimeoutError):
                    return False
            wait_until(ready, timeout=120)
            assert "native Python deployment is compatible" in log_path.read_text()
        finally:
            stop_process(process)


def test_real_alarm_restart_cleanup_and_denial(binary, tmp_path, fetch_server):
    project = tmp_path / "worker"
    init(project, binary)
    with (project / "worker.py").open("a") as output:
        output.write('''
from datetime import timedelta

class Reminder:
    def __init__(self, id: str, ctx: Context):
        self._ctx = ctx
    def schedule(self):
        self._ctx.alarms.set(timedelta(milliseconds=100))
    def alarm(self):
        self._ctx.storage.set("fired", True)
    def fired(self):
        return self._ctx.storage.get("fired", False)

def schedule(ctx: Context): return Reminder("test", ctx).schedule()
def fired(ctx: Context): return Reminder("test", ctx).fired()
async def upstream(ctx: Context, url: str):
    try:
        await ctx.fetch(url)
    except RuntimeError as error:
        return str(error)
    return "unexpected access"
''')
    worker = Worker(project, binary=binary)
    state_root = worker.root
    with worker:
        assert worker.call("increment") == 1
        worker.call("schedule")
        worker.wait_until(lambda: worker.call("fired"), timeout=15)
        fetch_server.respond("/api", {"ok": True})
        assert "outbound HTTP is disabled" in worker.call("upstream", {"url": fetch_server.url + "/api"})
        assert fetch_server.requests == []
        worker.restart()
        assert worker.call("increment") == 2
        assert worker.call("fired") is True
        process = worker.process
    assert process.poll() is not None
    assert not state_root.exists()


def test_invalid_source_is_rejected_before_dev_or_state(binary, tmp_path):
    project = tmp_path / "bad"
    init(project, binary)
    (project / "worker.py").write_text("def hello():\n    import requests\n")
    with pytest.raises(CheckError, match=r"worker.py:2:5: unsupported import"):
        check(project, binary)
    result = subprocess.run([sys.executable, "-m", "pycelld", "--binary", binary, "dev", project], capture_output=True, text=True, timeout=30)
    assert result.returncode == 1
    assert "worker.py:2:5" in result.stderr
    assert not (project / ".celld").exists()


def test_fixture_start_failure_cleans_up(binary, tmp_path):
    project = tmp_path / "bad"
    init(project, binary)
    (project / "worker.py").write_text("import requests\n")
    worker = Worker(project, binary=binary)
    state_root = worker.root
    with pytest.raises(CheckError):
        with worker:
            pytest.fail("invalid project was started")
    assert not state_root.exists()
