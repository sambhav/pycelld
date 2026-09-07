"""Number fork releases and publish verified assets without replacing a release."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib


def release_version(upstream, number):
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", upstream):
        raise ValueError("pin a stable upstream celld version before releasing")
    if not re.fullmatch(r"[1-9][0-9]*", str(number)):
        raise ValueError("release number must be a positive integer without leading zeros")
    return f"{upstream}-pycelld.{number}"


def release_number(tag):
    match = re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+-pycelld\.([1-9][0-9]*)", tag)
    return int(match[1]) if match else 0


def gh(*args):
    subprocess.run(["gh", *map(str, args)], check=True)


def api(path, *, missing_ok=False):
    result = subprocess.run(
        ["gh", "api", f"repos/{os.environ['GH_REPO']}/{path}"],
        capture_output=True, text=True,
    )
    if result.returncode:
        if missing_ok and "(HTTP 404)" in result.stderr:
            return None
        raise RuntimeError(result.stderr.strip())
    return json.loads(result.stdout)


def verify_assets(release, files):
    expected = {p.name: "sha256:" + hashlib.sha256(p.read_bytes()).hexdigest() for p in files}
    actual = {a["name"]: a.get("digest") for a in release["assets"]}
    if actual != expected:
        raise ValueError("release assets differ; published versions must never be overwritten")


def publish(assets):
    builds = json.loads((assets / "BUILD_INFO.json").read_text())
    number = int(os.environ["GITHUB_RUN_NUMBER"])
    source = os.environ["SOURCE_SHA"]
    upstream = builds[0]["upstream_version"]
    version = release_version(upstream, number)
    for build in builds:
        identity = (build["version"], build["upstream_version"],
                    build["build_number"], build["source_sha"])
        if identity != (version, upstream, number, source):
            raise ValueError("assets do not belong to this release")
    tag = "v" + version
    files = sorted(p for p in assets.iterdir() if p.is_file())
    existing = api(f"releases/tags/{tag}", missing_ok=True)
    ref = api(f"git/ref/tags/{tag}", missing_ok=True)
    if ref and api(f"commits/{tag}")["sha"] != source:
        raise ValueError("release tag points to a different source commit")
    if existing:
        if existing["target_commitish"] != source:
            raise ValueError("release belongs to a different source commit")
        if existing["draft"]:
            # Resume an interrupted upload while the release is still private.
            if set(a["name"] for a in existing["assets"]) - {p.name for p in files}:
                raise ValueError("draft contains unexpected assets")
            gh("release", "upload", tag, *files, "--clobber")
            verify_assets(api(f"releases/tags/{tag}"), files)
            gh("release", "edit", tag, "--draft=false", "--prerelease=false", "--latest=false")
        else:
            if not ref or existing["prerelease"]:
                raise ValueError("existing release is not the expected published release")
            verify_assets(existing, files)
    else:
        with tempfile.TemporaryDirectory() as directory:
            notes = Path(directory) / "notes.md"
            notes.write_text(
                f"celld **{upstream}** with native Monty, pycelld build **{number}**.\n\n"
                f"Source: `{source}`. Upstream: `{builds[0]['upstream_revision']}`.\n\n"
                "Python handlers, typed context, and durable objects run natively in Rust.\n\n"
                "All four native platform builds and the Rust, typing, HTTP/durability, "
                "and S3 conditional-write checks passed.\n\n"
                "Use `SHA256SUMS` to verify downloads. `BUILD_INFO.json` records both "
                "versions, source revisions, toolchain, and checksums. "
                "`celld.pyi` provides Python editor types. macOS binaries are not Apple-notarized.\n"
            )
            gh("release", "create", tag, "--target", source, "--latest=false",
               "--title", f"celld {version}", "--notes-file", notes, *files)

    # The workflow queues publication jobs. Builds may finish out of order;
    # retries of old runs must not move Latest backwards.
    latest = api("releases/latest", missing_ok=True)
    if latest is None or release_number(latest["tag_name"]) < number:
        gh("release", "edit", tag, "--latest")
    print(f"Published https://github.com/{os.environ['GH_REPO']}/releases/tag/{tag}")


if __name__ == "__main__":
    if sys.argv[1:] == ["version"]:
        upstream = tomllib.loads(Path("target/celld/crates/celld/Cargo.toml").read_text())["package"]["version"]
        print(release_version(upstream, os.environ["GITHUB_RUN_NUMBER"]))
    elif sys.argv[1:] == ["publish"]:
        publish(Path("assets"))
    else:
        raise SystemExit("usage: python3 tools/release.py <version|publish>")
