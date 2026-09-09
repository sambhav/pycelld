"""Install immutable, checksummed GitHub release binaries without Rust."""
from __future__ import annotations

import gzip
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
from urllib.request import Request, urlopen

REPOSITORY = "sambhav/pycelld"
MAX_DOWNLOAD = 512 * 1024 * 1024


def cache_dir() -> Path:
    return Path(os.environ.get("PYCELLD_CACHE_DIR", Path.home() / ".cache" / "pycelld"))


def platform_target() -> str:
    arch = {"x86_64": "x86_64", "AMD64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}.get(platform.machine())
    system = platform.system()
    if not arch or system not in ("Linux", "Darwin"):
        raise ValueError("celld binaries support Linux and macOS on x86-64/ARM64")
    if system == "Linux":
        libc, version = platform.libc_ver()
        if libc != "glibc" or tuple(map(int, version.split(".")[:2])) < (2, 28):
            raise ValueError("Linux celld binaries require glibc 2.28 or newer (musl/Alpine is unsupported)")
    return arch + ("-unknown-linux-gnu" if system == "Linux" else "-apple-darwin")


def download(url: str) -> bytes:
    request = Request(url, headers={"User-Agent": "pycelld-installer", "Accept": "application/vnd.github+json"})
    with urlopen(request, timeout=120) as response:
        data = response.read(MAX_DOWNLOAD + 1)
    if len(data) > MAX_DOWNLOAD:
        raise ValueError("release asset exceeds download limit")
    return data


def install(version: str = "latest") -> Path:
    target = platform_target()
    if version == "latest":
        release = json.loads(download(f"https://api.github.com/repos/{REPOSITORY}/releases/latest"))
        tag = release["tag_name"]
    else:
        tag = version if version.startswith("v") else "v" + version
    if not re.fullmatch(r"v\d+\.\d+\.\d+-pycelld\.\d+", tag):
        raise ValueError(f"invalid pycelld release tag: {tag!r}")
    root = cache_dir()
    root.mkdir(parents=True, exist_ok=True)
    destination = root / tag / target
    # Never execute or select an unverified partial download. Concurrent installs
    # each validate independently; immutable completed directories can be reused.
    with tempfile.TemporaryDirectory(prefix="install-", dir=root) as temporary:
        staging = Path(temporary)
        base = f"https://github.com/{REPOSITORY}/releases/download/{tag}"
        builds = json.loads(download(base + "/BUILD_INFO.json"))
        matches = [item for item in builds if item["target"] == target]
        if len(matches) != 1:
            raise ValueError(f"release manifest has no unique build for {target}")
        build = matches[0]
        if build["version"] != tag[1:] or build["repository"] != REPOSITORY:
            raise ValueError("release manifest does not match the requested repository/version")
        archive_name = f"celld-{target}.gz"
        if build["archive"] != archive_name:
            raise ValueError("unexpected release archive name")
        archive = download(base + "/" + archive_name)
        if hashlib.sha256(archive).hexdigest() != build["sha256"]:
            raise ValueError("release archive checksum mismatch")
        compressed = staging / archive_name
        compressed.write_bytes(archive)
        binary = staging / "celld"
        with gzip.open(compressed, "rb") as source, binary.open("wb") as output:
            total = 0
            while data := source.read(1024 * 1024):
                total += len(data)
                if total > MAX_DOWNLOAD:
                    raise ValueError("uncompressed binary exceeds size limit")
                output.write(data)
        if hashlib.sha256(binary.read_bytes()).hexdigest() != build["binary_sha256"]:
            raise ValueError("release binary checksum mismatch")
        binary.chmod(0o755)
        reported = subprocess.check_output([binary, "--version"], text=True, timeout=30).strip()
        if reported != "celld " + build["version"]:
            raise ValueError(f"binary reports unexpected version: {reported}")
        # Types come from the very binary being selected, including host extensions.
        subprocess.run([binary, "types", staging / "types"], check=True, timeout=30)
        compressed.unlink()
        (staging / "BUILD_INFO.json").write_text(json.dumps(build, indent=2) + "\n")
        destination.parent.mkdir(parents=True, exist_ok=True)
        def verify_existing() -> None:
            if (destination / "celld").is_symlink() or hashlib.sha256((destination / "celld").read_bytes()).hexdigest() != build["binary_sha256"]:
                raise ValueError(f"existing installation was modified: {destination}; remove it and retry")
        if destination.exists():
            verify_existing()
        else:
            pending = destination.with_name(target + ".pending-" + staging.name)
            try:
                shutil.copytree(staging, pending)
                try:
                    pending.rename(destination)
                except OSError as error:
                    if error.errno not in (errno.EEXIST, errno.ENOTEMPTY):
                        raise
                    # Another installer finished the same immutable release.
                    # Verify the winner before selecting it, just as on reuse.
                    verify_existing()
            finally:
                if pending.exists():
                    shutil.rmtree(pending)
        selection = staging / "active.json"
        selection.write_text(json.dumps({"tag": tag, "target": target}) + "\n")
        os.replace(selection, root / "active.json")
    return destination / "celld"


def resolve_binary(binary: str | Path | None = None) -> Path:
    explicit = binary or os.environ.get("PYCELLD_BINARY")
    if explicit:
        path = Path(shutil.which(str(explicit)) or explicit).expanduser().resolve()
    else:
        selected = cache_dir() / "active.json"
        if selected.is_file():
            selection = json.loads(selected.read_text())
            path = cache_dir() / selection["tag"] / selection["target"] / "celld"
        elif found := shutil.which("celld"):
            path = Path(found)
        else:
            raise ValueError("celld is not installed; run `pycelld install`, or set PYCELLD_BINARY")
    if not path.is_file() or not os.access(path, os.X_OK):
        raise ValueError(f"celld binary is missing or not executable: {path}; run `pycelld install`")
    return path.resolve()
