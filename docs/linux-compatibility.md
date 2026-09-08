# Linux binary compatibility

The published GNU/Linux binaries target **glibc 2.28 or later** on x86-64 and
ARM64. This covers the glibc baseline in RHEL/AlmaLinux/Rocky Linux 8 and newer
Debian/Ubuntu releases. It does not cover musl-only systems such as Alpine,
glibc before 2.28, or arbitrary older kernels. Container tests use the runner's
kernel and verify the userspace ABI, not a minimum kernel version.

Release builds use PyPA's manylinux 2.28 compiler images, pinned by digest in
[`build.sh`](../tools/linux-checks/build.sh). They include a modern compiler that
targets the older C library. Building on a recent Ubuntu host and merely changing
a version label would retain that host's glibc requirement.

All Cargo compilation and final linking happen inside the compiler container;
only the pinned Rust toolchain and download/build caches are shared with the
runner. Linux build caches have a separate glibc-2.28 prefix. This also avoids
reusing C/C++ objects previously built against the runner's newer libc. Ordinary
local `cargo build` continues to use the local system's libc.

Before packaging, CI:

1. Inspects the final ELF architecture, interpreter, shared-library dependencies
   and symbol versions. Requirements newer than `GLIBC_2.28`, private glibc ABIs,
   unexpected shared libraries and RPATH/RUNPATH entries fail the release.
2. Runs the exact binary in a separate AlmaLinux 8.10 container: V8 HTTP and
   durable SQLite across restart, the Monty HTTP/durability suite and the S3
   protocol tests. This container has no compiler toolset or custom library path.
3. Runs the existing checks on the newer GitHub runner too. Packaging and release
   assembly verify that the audited binary hash matches the published archive.

`BUILD_INFO.json` contains `linux_abi` with the configured baseline, actual
maximum glibc symbol version, dynamic loader, library list and binary hash.
macOS entries have `linux_abi: null`.

## Reproduce a release build

Use a native Linux x86-64 or ARM64 machine with Docker and Rust 1.98.1 installed
through rustup. Run from the repository root:

```sh
export GITHUB_RUN_NUMBER=1  # local distribution version suffix
export CARGO_BUILD_JOBS=2
bash tools/linux-checks/build.sh x86_64-unknown-linux-gnu
bash tools/linux-checks/check.sh x86_64-unknown-linux-gnu
```

Use `aarch64-unknown-linux-gnu` on ARM64. The check script needs Python 3 and
`readelf` (binutils) on the host; it installs Python for the test harness inside
the compatibility container. Neither script publishes a release.

When updating the Rust toolchain, V8 or any native dependency, keep these gates
in place. A successful build alone does not establish the supported ABI.
