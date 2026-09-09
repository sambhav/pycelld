import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import errno
import shutil

import pytest

from pycelld import install as installer


def release(monkeypatch, tmp_path):
    target = "x86_64-unknown-linux-gnu"
    binary = b"verified binary bytes"
    archive = gzip.compress(binary)
    build = {"target": target, "version": "0.4.1-pycelld.42", "repository": "sambhav/pycelld", "archive": f"celld-{target}.gz", "sha256": hashlib.sha256(archive).hexdigest(), "binary_sha256": hashlib.sha256(binary).hexdigest()}
    assets = {"latest": json.dumps({"tag_name": "v0.4.1-pycelld.42"}).encode(), "BUILD_INFO.json": json.dumps([build]).encode(), build["archive"]: archive}
    monkeypatch.setenv("PYCELLD_CACHE_DIR", str(tmp_path))
    monkeypatch.setattr(installer, "platform_target", lambda: target)
    monkeypatch.setattr(installer, "download", lambda url: assets[url.rsplit("/", 1)[1]])
    monkeypatch.setattr(subprocess, "check_output", lambda *a, **kw: "celld 0.4.1-pycelld.42\n")
    calls = []
    def types(command, **kwargs):
        calls.append(command)
        output = Path(command[2])
        output.mkdir()
        (output / "celld.pyi").write_text("# types from the chosen binary\n")
    monkeypatch.setattr(subprocess, "run", types)
    return assets, build, calls


def test_atomic_binary_and_matching_types_install(monkeypatch, tmp_path):
    assets, build, calls = release(monkeypatch, tmp_path)
    path = installer.install()
    assert path.read_bytes() == b"verified binary bytes"
    assert (path.parent / "types/celld.pyi").read_text() == "# types from the chosen binary\n"
    assert installer.resolve_binary() == path
    assert calls[0][1] == "types"
    assert not list(tmp_path.glob("install-*"))


def test_bad_archive_never_replaces_active_install(monkeypatch, tmp_path):
    assets, build, calls = release(monkeypatch, tmp_path)
    path = installer.install()
    previous = (tmp_path / "active.json").read_bytes()
    assets[build["archive"]] = b"corrupt"
    with pytest.raises(ValueError, match="archive checksum mismatch"):
        installer.install()
    assert (tmp_path / "active.json").read_bytes() == previous
    assert path.read_bytes() == b"verified binary bytes"
    assert len(calls) == 1


def test_binary_hash_is_checked_before_execution(monkeypatch, tmp_path):
    assets, build, calls = release(monkeypatch, tmp_path)
    build["binary_sha256"] = "0" * 64
    assets["BUILD_INFO.json"] = json.dumps([build]).encode()
    with pytest.raises(ValueError, match="binary checksum mismatch"):
        installer.install()
    assert calls == [] and not (tmp_path / "active.json").exists()


def test_concurrent_install_verifies_and_reuses_completed_winner(monkeypatch, tmp_path):
    release(monkeypatch, tmp_path)
    original = Path.rename
    def race(path, destination):
        if ".pending-" in path.name:
            shutil.copytree(path, destination)
            raise OSError(errno.ENOTEMPTY, "another installer completed")
        return original(path, destination)
    monkeypatch.setattr(Path, "rename", race)
    path = installer.install()
    assert installer.resolve_binary() == path
    assert not list(path.parent.parent.glob("*.pending-*"))


@pytest.mark.parametrize("tag", ["../../other", "v0.4.1", "v0.4.1-pycelld.dev"])
def test_release_tag_cannot_escape_cache(monkeypatch, tmp_path, tag):
    release(monkeypatch, tmp_path)
    with pytest.raises(ValueError, match="release tag"):
        installer.install(tag)


def test_platform_guidance(monkeypatch):
    monkeypatch.setattr(installer.platform, "system", lambda: "Linux")
    monkeypatch.setattr(installer.platform, "machine", lambda: "aarch64")
    monkeypatch.setattr(installer.platform, "libc_ver", lambda: ("glibc", "2.28"))
    assert installer.platform_target() == "aarch64-unknown-linux-gnu"
    monkeypatch.setattr(installer.platform, "libc_ver", lambda: ("musl", "1.2"))
    with pytest.raises(ValueError, match="glibc 2.28"):
        installer.platform_target()
