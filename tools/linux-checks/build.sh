#!/usr/bin/env bash
# Native build against glibc 2.28, including every C/C++ dependency and linker.
set -euo pipefail
cd "$(dirname "$0")/../.."
target=${1:?usage: build.sh TARGET}
case "$target:$(uname -m)" in
  x86_64-unknown-linux-gnu:x86_64)
    image=quay.io/pypa/manylinux_2_28_x86_64@sha256:53390351aeb4688114b02c36a23b3e6ce1166ee9b7afc5df1a4f776354fc764c ;;
  aarch64-unknown-linux-gnu:aarch64)
    image=quay.io/pypa/manylinux_2_28_aarch64@sha256:ad74e53b713f3b07d8c889c526dc0c6500da9827b45e38739570875fef52e28f ;;
  *) echo "TARGET must match a native Linux x86-64 or ARM64 host" >&2; exit 1 ;;
esac
# PyPA manylinux 2026.09.05-1. Keep caches isolated from host-glibc builds.
# Build as the caller so Cargo outputs and caches remain writable on the host.
# Rust is installed on the runner; its toolchain runs on glibc 2.28 too.
docker run --rm --user "$(id -u):$(id -g)" \
  -v "$PWD:$PWD" -w "$PWD" \
  -v "$HOME/.cargo:/cargo" -v "$HOME/.rustup:/rustup:ro" \
  -e CARGO_HOME=/cargo -e RUSTUP_HOME=/rustup \
  -e GIT_CONFIG_GLOBAL=/tmp/pycelld-gitconfig \
  -e TARGET="$target" -e CARGO_BUILD_JOBS -e GITHUB_RUN_NUMBER \
  "$image" bash -euo pipefail -c '
    export PATH="/cargo/bin:/opt/python/cp312-cp312/bin:$PATH"
    test "$(getconf GNU_LIBC_VERSION)" = "glibc 2.28"
    test "$(rustc -vV | sed -n "s/^host: //p")" = "$TARGET"
    git config --global --add safe.directory "$PWD"
    git config --global --add safe.directory "$PWD/target/celld"
    cargo xtask prepare
    export CELLD_RELEASE_VERSION="$(python3 tools/release.py version)"
    cargo test --manifest-path tools/packing-checks/Cargo.toml --locked
    cargo xtask build --target "$TARGET" --release
  '
