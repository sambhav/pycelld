"""Build a separate pycelld consumer, then check generated types and real HTTP.

CI uses a Git dependency on the checked-out commit. --path tests uncommitted work.
Neither application mode runs xtask or applies a patch.
"""
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen
import argparse
import json
import os
import shutil
import socket
import subprocess
import tempfile
import time
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--path", action="store_true")
parser.add_argument("--profile", choices=["dev", "lab"], default="lab")
args = parser.parse_args()
repo = Path(__file__).resolve().parents[2]
cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")

with tempfile.TemporaryDirectory(prefix="pycelld-consumer-") as directory:
    root = Path(directory)
    project = root / "app"
    shutil.copytree(repo / "examples/extended-host", project)
    if args.path:
        dependency = f"path = {json.dumps(str(repo))}"
    else:
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip()
        dependency = f"git = {json.dumps(repo.as_uri())}, rev = {json.dumps(revision)}"
    # Matching build profiles reuse dependency artifacts, but the application
    # is outside every pycelld workspace and imports only its public crate.
    profiles = repo.joinpath("Cargo.toml").read_text().split("[profile.release]", 1)[1]
    root.joinpath("Cargo.toml").write_text(f'''[package]
name = "pycelld-consumer"
version = "0.1.0"
edition = "2024"
[dependencies]
pycelld = {{ {dependency} }}
[[bin]]
name = "pycelld-consumer"
path = "app/main.rs"
[profile.release]{profiles}
''')
    shutil.copyfile(repo / "Cargo.lock", root / "Cargo.lock")
    target = repo / "target"
    subprocess.run([cargo, "build", "--profile", args.profile, "--target-dir", str(target)], cwd=root, check=True)
    binary = target / ("debug" if args.profile == "dev" else "lab") / "pycelld-consumer"
    types = root / "types"
    types.mkdir()
    subprocess.run([binary, "types", types], check=True)
    assert types.joinpath("acme/native.pyi").is_file()
    assert types.joinpath("acme/greeters.pyi").is_file()
    env = dict(os.environ, MYPYPATH=str(types))
    subprocess.run(["python", "-m", "mypy", "--strict", str(project / "worker"), str(project / "helpers.py")], env=env, cwd=root, check=True)
    # A negative type check proves the extension declarations are being used.
    wrong = root / "wrong.py"
    wrong.write_text("from acme.native import shout\nshout(123)\n")
    check = subprocess.run(["python", "-m", "mypy", "--strict", str(wrong)], env=env, cwd=root, capture_output=True, text=True)
    assert check.returncode == 1 and "arg-type" in check.stdout, check.stdout + check.stderr

    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    url = f"http://127.0.0.1:{port}"
    env = {key: value for key, value in os.environ.items() if not key.startswith("CELLD_")}
    env["CELLD_MAX_STATELESS_ISOLATES"] = "2"
    log_path = root / "server.log"

    def call(name, values):
        request = Request(url + "/" + name, data=json.dumps(values).encode(), headers={"content-type": "application/json"})
        try:
            with urlopen(request, timeout=30) as response:
                data = response.read()
                return json.loads(data) if "application/json" in response.headers.get("content-type", "") else data.decode()
        except HTTPError as error:
            if error.code != 404:
                raise AssertionError(f"{name}: {error.code}: {error.read().decode()}") from error
            raise

    with log_path.open("w") as log:
        process = subprocess.Popen([binary, "dev", project, "--port", str(port), "--logs"], env=env, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 120
            while True:
                assert process.poll() is None, log_path.read_text()
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                        break
                except OSError:
                    assert time.monotonic() < deadline, log_path.read_text()
                    time.sleep(0.1)
            greeting = call("hello", {"name": "Ada"})
            assert greeting["message"] == "HELLO, ADA!", greeting
            uuid.UUID(greeting["request_id"])
            assert call("quiet", {"text": "Ada"}) == "ADA"
            greeting = call("greet_room", {"id": "room/1", "name": "Grace"})
            assert greeting["message"] == "HELLO, GRACE!", greeting
            uuid.UUID(greeting["request_id"])
            assert call("last", {"id": "room/1"}) == "HELLO, GRACE!"
            for name in ["shout", "Greeter", "Greeting"]:
                try:
                    call(name, {})
                except HTTPError as error:
                    assert error.code == 404, error.read()
                else:
                    raise AssertionError(f"extension {name} was exposed as an HTTP handler")
        except BaseException:
            print(log_path.read_text())
            raise
        finally:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
print("Downstream pycelld import, extension types, native callbacks, classes, HTTP and durable storage passed")
