import json

import pytest

from pycelld.check import CheckError, preflight, read_config


def project(tmp_path, code, config=None):
    (tmp_path / "worker.py").write_text(code)
    (tmp_path / "wrangler.jsonc").write_text(json.dumps(config or {"name": "test", "main": "worker.py"}, indent=2))
    return tmp_path


def test_import_syntax_and_capability_diagnostics_have_source_locations(tmp_path):
    root = project(tmp_path, "from celld import Context\n\ndef run():\n    import requests\n    yield 1\n", {"name": "test", "main": "worker.py", "queues": {"producers": [{"binding": "JOBS", "queue": "jobs"}]}, "imaginary": True})
    errors = preflight(root)
    assert any(e.path.name == "worker.py" and e.line == 4 and e.column == 5 and "requests" in e.message for e in errors)
    assert any(e.line == 5 and "generators" in e.message for e in errors)
    assert any(e.path.name == "wrangler.jsonc" and e.line == 4 and "queues" in e.message for e in errors)
    assert any("imaginary" in e.message for e in errors)
    (root / "worker.py").write_text("def run(:\n    pass\n")
    assert any(e.path.name == "worker.py" and e.line == 1 and e.column > 1 for e in preflight(root))


def test_jsonc_preserves_strings_comments_trailing_commas_and_diagnostics(tmp_path):
    path = tmp_path / "wrangler.jsonc"
    path.write_text('''{
  // config
  "name": "test", /* comment */
  "main": "worker.py",
  "vars": {"URL": "https://example.com/a,}",},
}''')
    assert read_config(tmp_path)[2]["vars"]["URL"] == "https://example.com/a,}"
    path.write_text('{\n // comment\n "name": bad\n}')
    with pytest.raises(CheckError, match=r"wrangler.jsonc:3:10:"):
        read_config(tmp_path)


def test_config_location_ignores_commented_keys_and_empty_capabilities(tmp_path):
    project(tmp_path, "def run(): return 1\n")
    config = tmp_path / "wrangler.jsonc"
    config.write_text('{\n// "unknown": 1\n"name": "test", "main": "worker.py",\n"unknown": true\n}')
    errors = preflight(tmp_path)
    assert len(errors) == 1 and errors[0].line == 4
    project(tmp_path, "def run(): return 1\n", {"name": "test", "main": "worker.py", "queues": {"producers": []}, "triggers": {"crons": []}})
    assert preflight(tmp_path) == []


def test_follows_only_reachable_sources_and_recognizes_host_extensions(tmp_path):
    root = project(tmp_path, "from shop import run\nfrom acme.native import shout\n")
    (root / "shop").mkdir()
    (root / "shop/__init__.py").write_text("from .api import run\n")
    (root / "shop/api.py").write_text("from .values import answer\ndef run(): return answer\n")
    (root / "shop/values.py").write_text("answer = 42\n")
    (root / "test_ignored.py").write_text("import pytest\n")
    types = root / "types"
    (types / "acme").mkdir(parents=True)
    (types / "acme/native.pyi").write_text("def shout(text: str) -> str: ...\n")
    assert preflight(root, types) == []
    (root / "shop/values.py").write_text("import missing\n")
    errors = preflight(root, types)
    assert len(errors) == 1 and errors[0].path.name == "values.py"


def test_package_entries_check_even_unimported_modules(tmp_path):
    project(tmp_path, "", {"name": "test", "main": "shop"})
    (tmp_path / "shop").mkdir()
    (tmp_path / "shop/__init__.py").write_text("def run(): return 1\n")
    (tmp_path / "shop/unused.py").write_text("import requests\n")
    assert any(e.path.name == "unused.py" for e in preflight(tmp_path))


def test_does_not_execute_project_python(tmp_path):
    root = project(tmp_path, "raise RuntimeError('not executed')\ndef run(): return 1\n")
    assert preflight(root) == []


def test_source_symlink_cannot_escape_project(tmp_path):
    root = tmp_path / "app"
    root.mkdir()
    project(root, "import outside\n")
    (tmp_path / "outside.py").write_text("value = 42\n")
    (root / "outside.py").symlink_to(tmp_path / "outside.py")
    assert "escapes the project" in preflight(root)[0].message
