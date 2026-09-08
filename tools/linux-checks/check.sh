#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
target=${1:?usage: check.sh TARGET}
case "$target:$(uname -m)" in
  x86_64-unknown-linux-gnu:x86_64|aarch64-unknown-linux-gnu:aarch64) ;;
  *) echo "TARGET must match the native host" >&2; exit 1 ;;
esac
binary="target/$target/release/celld"
python3 tools/linux-checks/elf.py "$binary" --target "$target" --output target/linux-abi.json
# Test the exact binary with ordinary distribution libraries. The compiler
# image's newer GCC runtimes must not hide a deployment dependency.
docker run --rm -v "$PWD:$PWD:ro" -w "$PWD" -e TARGET="$target" \
  almalinux:8.10 bash -euo pipefail -c '
    dnf -q install -y python3.12 ca-certificates
    test "$(getconf GNU_LIBC_VERSION)" = "glibc 2.28"
    binary="target/$TARGET/release/celld"
    "$binary" --version
    python3.12 tools/linux-checks/v8.py "$binary"
    python3.12 tools/monty-checks/e2e.py "$binary"
    python3.12 tools/storage-checks/probe.py "$binary" -v
  '
