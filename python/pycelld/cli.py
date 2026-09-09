"""Small Python-facing CLI; deployment and execution stay in celld."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

from .check import check
from .install import install, resolve_binary

WORKER = '''from celld import Context


class Counter:
    def __init__(self, id: str, ctx: Context):
        self._ctx = ctx

    def increment(self) -> int:
        value = self._ctx.storage.get("count", 0)
        assert isinstance(value, int)
        self._ctx.storage.set("count", value + 1)
        self._ctx.storage.sync()
        return value + 1


def hello(name: str = "world") -> str:
    return f"Hello, {name}!"


def increment(ctx: Context, id: str = "visits") -> int:
    return Counter(id, ctx).increment()
'''

TEST = '''from pathlib import Path

PROJECT = Path(__file__).resolve().parents[1]


def test_durable_counter(celld_worker):
    worker = celld_worker(PROJECT)
    assert worker.call("hello", {"name": "Sam"}) == "Hello, Sam!"
    assert worker.call("increment") == 1
    worker.restart()
    assert worker.call("increment") == 2


def test_isolated_state(celld_worker):
    worker = celld_worker(PROJECT)
    assert worker.call("increment") == 1
'''


def init(project: Path, binary: str | Path | None = None) -> None:
    if project.exists() and (not project.is_dir() or any(project.iterdir())):
        raise ValueError(f"{project} must be an empty or new directory; init never overwrites files")
    name = re.sub(r"[^a-z0-9]+", "-", project.resolve().name.lower()).strip("-")[:63].rstrip("-") or "python-worker"
    with tempfile.TemporaryDirectory(prefix="pycelld-init-") as directory:
        staging = Path(directory)
        subprocess.run([resolve_binary(binary), "types", staging], check=True, timeout=30)
        (staging / "worker.py").write_text(WORKER)
        (staging / "wrangler.jsonc").write_text(json.dumps({"name": name, "main": "worker.py"}, indent=2) + "\n")
        (staging / "pyproject.toml").write_text('[tool.pytest.ini_options]\ntestpaths = ["tests"]\n\n[tool.mypy]\nstrict = true\nfiles = ["worker.py"]\n')
        (staging / "tests").mkdir()
        (staging / "tests/conftest.py").write_text('pytest_plugins = ["pycelld.pytest_plugin"]\n')
        (staging / "tests/test_worker.py").write_text(TEST)
        (staging / ".gitignore").write_text(".celld/\n.venv/\n__pycache__/\n.pytest_cache/\n.mypy_cache/\n")
        shutil.copytree(staging, project, dirs_exist_ok=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Install, develop and test native Python workers")
    parser.add_argument("--binary", help="celld binary override (also PYCELLD_BINARY)")
    commands = parser.add_subparsers(dest="command", required=True)
    installer = commands.add_parser("install", help="download a verified release binary for this machine")
    installer.add_argument("--version", default="latest", help="exact release version or latest")
    initializer = commands.add_parser("init", help="create a Python project with matching types and pytest tests")
    initializer.add_argument("project", type=Path)
    for name in ("check", "dev"):
        command = commands.add_parser(name, help="validate before " + ("deployment" if name == "check" else "serving locally"))
        command.add_argument("project", nargs="?", type=Path, default=Path("."))
        if name == "dev":
            command.add_argument("--port", type=int, default=9876)
            command.add_argument("--logs", action="store_true")
    types = commands.add_parser("types", help="refresh editor types from the selected binary")
    types.add_argument("directory", nargs="?", type=Path, default=Path("."))
    args = parser.parse_args(argv)
    try:
        if args.command == "install":
            print(f"Installed {install(args.version)}")
        elif args.command == "init":
            init(args.project, args.binary)
            print(f"Created {args.project}. Run `pycelld dev {args.project}`.")
        elif args.command == "types":
            subprocess.run([resolve_binary(args.binary), "types", args.directory], check=True, timeout=30)
        else:
            check(args.project, args.binary)
            print(f"Checked {args.project}: native Python deployment is compatible.", flush=True)
            if args.command == "dev":
                command = [str(resolve_binary(args.binary)), "dev", str(args.project), "--port", str(args.port)]
                if args.logs:
                    command.append("--logs")
                # Replace the wrapper so signals/reload/cleanup use the native CLI.
                import os
                os.execv(command[0], command)
        return 0
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
