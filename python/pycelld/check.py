"""Source diagnostics followed by celld's actual deployment compiler.

This is a compatibility preflight, not a second interpreter or a proof that
every possible handler input succeeds. No project Python code runs in CPython.
"""
from __future__ import annotations

import ast
from dataclasses import dataclass
import json
from pathlib import Path
import re
import subprocess
import tempfile

from .install import resolve_binary

# Monty's built-ins (kept aligned with package::reserved), plus future annotations.
BUILTINS = frozenset("celld asyncio base64 binascii builtins collections dataclasses datetime functools gc itertools json math os pathlib re sys typing unicodedata __future__".split())
CONFIG_KEYS = frozenset("name main compatibility_date compatibility_flags durable_objects migrations assets services triggers vars d1_databases kv_namespaces queues workflows r2_buckets no_bundle".split())


@dataclass(frozen=True)
class Diagnostic:
    path: Path
    line: int
    column: int
    message: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}:{self.column}: {self.message}"


class CheckError(ValueError):
    pass


def read_config(project: Path) -> tuple[Path, str, dict]:
    if project.is_file():
        config = project
    else:
        config = next((project / name for name in ("wrangler.jsonc", "wrangler.json") if (project / name).is_file()), project / "wrangler.jsonc")
    source = config.read_text()
    # Preserve strings and source offsets, including comment-like text in URLs.
    def replace(match: re.Match) -> str:
        token = match.group()
        if token.startswith('"'):
            return token
        return "".join("\n" if c == "\n" else " " for c in token)
    stripped = re.sub(r'"(?:\\.|[^"\\])*"|//[^\n]*|/\*[\s\S]*?\*/', replace, source)
    stripped = re.sub(r'"(?:\\.|[^"\\])*"|,\s*(?=[}\]])', replace, stripped)
    try:
        config_data = json.loads(stripped)
    except json.JSONDecodeError as error:
        raise CheckError(str(Diagnostic(config, error.lineno, error.colno, error.msg))) from error
    if not isinstance(config_data, dict):
        raise CheckError(str(Diagnostic(config, 1, 1, "configuration must be an object")))
    return config, stripped, config_data


def preflight(project: Path, type_root: Path | None = None) -> list[Diagnostic]:
    config, source, data = read_config(project)
    root = config.parent.resolve()
    diagnostics: list[Diagnostic] = []

    def config_error(key: str, message: str) -> None:
        match = re.search(r'"' + re.escape(key) + r'"\s*:', source)
        offset = match.start() if match else 0
        diagnostics.append(Diagnostic(config, source.count("\n", 0, offset) + 1, offset - source.rfind("\n", 0, offset), message))

    for key in data.keys() - CONFIG_KEYS:
        config_error(key, f"unsupported configuration `{key}`; remove it or use a host that supports it")
    for key in ("services", "d1_databases", "kv_namespaces", "workflows", "r2_buckets"):
        if data.get(key):
            config_error(key, f"native Python does not expose `{key}` bindings yet; use object storage/SQL or a TypeScript worker")
    if isinstance(data.get("queues"), dict) and any(data["queues"].values()):
        config_error("queues", "native Python does not expose `queues` bindings yet; use a TypeScript worker")
    if isinstance(data.get("triggers"), dict) and any(data["triggers"].values()):
        config_error("triggers", "native Python scheduled handlers are not supported yet")
    if isinstance(data.get("assets"), dict) and data["assets"].get("binding"):
        config_error("assets", "native Python does not expose asset bindings; static asset routing remains supported")
    if isinstance(data.get("durable_objects"), dict) and data["durable_objects"].get("bindings"):
        config_error("durable_objects", "Python discovers durable classes automatically; remove explicit bindings")
    if data.get("no_bundle"):
        config_error("no_bundle", "native Python must be bundled; remove no_bundle")
    main = data.get("main")
    if not isinstance(main, str):
        config_error("main", "set `main` to a Python file or package directory")
        return diagnostics
    entry = (root / main).resolve()
    if not entry.is_relative_to(root):
        config_error("main", "Python entry must stay inside the project")
        return diagnostics
    directory = entry.is_dir() or entry.name == "__init__.py"
    package = entry if entry.is_dir() else entry.parent
    base = package.parent if directory else package
    while base.is_relative_to(root) and (base / "__init__.py").is_file():
        base = base.parent
    entry_file = package / "__init__.py" if directory else entry
    pending = list(package.rglob("*.py")) if directory else [entry_file]
    visited: set[Path] = set()

    def locate(name: str) -> Path | None:
        candidate = base.joinpath(*name.split("."))
        if (candidate / "__init__.py").is_file():
            return candidate / "__init__.py"
        if candidate.with_suffix(".py").is_file():
            return candidate.with_suffix(".py")
        return None

    def provided(name: str) -> bool:
        if name.split(".")[0] in BUILTINS:
            return True
        if type_root:
            path = type_root.joinpath(*name.split("."))
            return path.with_suffix(".pyi").is_file() or path.is_dir() and any(path.rglob("*.pyi"))
        return False

    while pending:
        path = pending.pop()
        if path in visited:
            continue
        visited.add(path)
        if len(visited) > 256:
            diagnostics.append(Diagnostic(path, 1, 1, "Python packages are limited to 256 modules"))
            break
        if not path.resolve().is_relative_to(root):
            diagnostics.append(Diagnostic(path, 1, 1, "Python source escapes the project"))
            continue
        try:
            code = path.read_text()
            tree = ast.parse(code, filename=str(path))
        except SyntaxError as error:
            diagnostics.append(Diagnostic(path, error.lineno or 1, error.offset or 1, error.msg))
            continue
        except OSError as error:
            diagnostics.append(Diagnostic(path, 1, 1, str(error)))
            continue
        relative = path.relative_to(base).with_suffix("")
        module = list(relative.parts)
        if module[-1] == "__init__":
            module.pop()
            parent = module
        else:
            parent = module[:-1]
        # Parent __init__ modules run when imports enter their package too.
        for depth in range(1, len(module)):
            ancestor = locate(".".join(module[:depth]))
            if ancestor:
                pending.append(ancestor)
        for node in ast.walk(tree):
            def error(message: str) -> None:
                location = node.iter if isinstance(node, ast.comprehension) else node
                diagnostics.append(Diagnostic(path, getattr(location, "lineno", 1), getattr(location, "col_offset", 0) + 1, message))
            if isinstance(node, (ast.Yield, ast.YieldFrom)):
                error("Monty does not support generators; return a list or a buffered Response")
            elif isinstance(node, ast.AsyncFor) or isinstance(node, ast.comprehension) and node.is_async:
                error("Monty does not support async iteration; await a buffered result")
            if isinstance(node, ast.Import):
                names = [alias.name for alias in node.names]
            elif isinstance(node, ast.ImportFrom):
                if any(alias.name == "*" for alias in node.names):
                    error("use named imports instead of import *")
                if node.level > len(parent) or node.level and not parent:
                    error("relative import escapes its package")
                    continue
                prefix = parent[:len(parent) - node.level + 1] if node.level else []
                target = ".".join(prefix + ([node.module] if node.module else []))
                names = [target]
                for alias in node.names:
                    child = locate(target + "." + alias.name)
                    if child:
                        pending.append(child)
            else:
                continue
            for name in names:
                local = locate(name)
                if local:
                    pending.append(local)
                elif not provided(name):
                    error(f"unsupported import `{name}`; Monty cannot load CPython packages. Use supported modules, project sources, or host extensions (HTTP: ctx.fetch)")
    return sorted(diagnostics, key=lambda item: (str(item.path), item.line, item.column))


def check(project: Path, binary: str | Path | None = None, *, env: dict[str, str] | None = None) -> str:
    executable = resolve_binary(binary)
    with tempfile.TemporaryDirectory(prefix="pycelld-types-") as directory:
        # Query extensions from the selected runtime rather than trusting stale
        # editor files in the application directory.
        types = subprocess.run([executable, "types", directory], capture_output=True, text=True, env=env, timeout=30)
        if types.returncode:
            raise CheckError(f"selected celld host failed to load:\n{types.stderr or types.stdout}")
        diagnostics = preflight(project, Path(directory))
    if diagnostics:
        raise CheckError("\n".join(map(str, diagnostics)))
    result = subprocess.run([executable, "deploy", str(project.resolve()), "--dry-run", "--json"], capture_output=True, text=True, env=env, timeout=120)
    if result.returncode:
        raise CheckError(f"{project}: native deployment check failed:\n{result.stderr or result.stdout}")
    return result.stdout
