# Host configuration and native binaries

The host-options patch adds optional CPU cell density and S3 ETag spelling settings to celld
0.4.1. Neither changes the storage protocol or adds an external dependency.

## Ceph and S3 ETag compatibility

celld 0.4.0 already accepts bare ETags and preserves their exact spelling. Current
[Ceph S3 responses quote ETags](https://github.com/ceph/ceph/blob/main/src/rgw/rgw_rest.cc)
and its [RADOS precondition checks accept either spelling](https://github.com/ceph/ceph/blob/main/src/rgw/driver/rados/rgw_rados.cc).
A Ceph release, S3 gateway, or proxy can nevertheless expose inconsistent spelling
between responses and the conditional headers it accepts. This is an explicit
compatibility option for that mismatch, not a requirement for every Ceph cluster.

Set `CELLD_S3_ETAG_MODE` on celld processes that access the affected endpoint:

| Mode | Behavior |
| --- | --- |
| `preserve` (default) | Keep the provider's token exactly as returned. |
| `unquoted` | Remove one balanced pair of surrounding quotes, for gateways that require bare `If-Match` tokens. |
| `quoted` | Add one pair of quotes to bare tokens, for gateways that require quoted `If-Match` tokens. |

For an endpoint that requires bare tokens:

```sh
export CELLD_S3_ETAG_MODE=unquoted
celld diagnose --bucket my-bucket --endpoint https://my-rgw.example --json
celld --bucket my-bucket --endpoint https://my-rgw.example
```

Credentials use the normal AWS environment/provider chain. Use `quoted` instead
if the endpoint returns bare tokens but requires quotes on conditional writes.
Use the same setting across processes accessing the affected bucket. It is read
when a bucket client is created; restart existing nodes after changing it.

The mode normalizes S3 CAS tokens from GET, HEAD and successful writes, and the
tokens used for conditional updates. It preserves the token value, including
multipart suffixes and case. GCS generation tokens, Azure tokens, and the local
development store are unaffected. Worker-facing R2 condition expressions retain
their existing semantics.

Compatibility modes reject empty tokens, weak validators, wildcard tokens,
lists, whitespace, and malformed quotes. They never drop an `If-Match` condition,
retry a failed write without its condition, or disable the startup storage probe.
A store that ignores conditional writes remains unsupported: the diagnostic and
startup probe must still reject it. A failed CAS response may have committed;
CAS retries stay disabled.

The regression tests use strict S3-compatible HTTP fixtures with the actual
celld binary. They cover both spelling mismatches, default behavior, stale
writes, ambiguous responses, and unsafe tokens. They do not claim validation
against a live Ceph deployment.

```sh
cargo xtask prepare
cargo test --manifest-path tools/packing-checks/Cargo.toml --locked
python3 tools/storage-checks/probe.py target/release/celld -v
```

## Download and release binaries

The **Native binaries** workflow builds native release binaries for:

- Linux x86-64 and ARM64 (GNU/glibc, built on Ubuntu 22.04).
- macOS Intel and Apple Silicon (built on macOS 15).

Every push to `main` builds and publishes a normal GitHub release after all four
platforms and the Rust, typing, and HTTP validation suite pass. No manual version
bump or tag is needed. You can also run **Native binaries** manually from `main`
in GitHub Actions. Label a PR `build-binaries` to build downloadable review
artifacts; PRs do not publish releases.

Download the [latest pycelld release](https://github.com/sambhav/pycelld/releases/latest).
Tags use `v<upstream-version>-pycelld.<build-number>`:

| Upstream celld | Workflow run | Published version |
| --- | ---: | --- |
| `0.4.1` | 4 | `0.4.1-pycelld.4` |
| `0.4.1` | 5 | `0.4.1-pycelld.5` |
| `0.4.2` | 6 | `0.4.2-pycelld.6` |

The build number is GitHub's `run_number` for **Native binaries**. It increases
automatically and does not reset when upstream changes. Failed runs, PR builds,
and manual runs can leave gaps. Rerunning a workflow keeps the same number.

The suffix identifies this distribution and orders its builds numerically.
[SemVer](https://semver.org/#spec-item-9) places a suffixed version before the
corresponding plain upstream version; compare fork versions within pycelld.
These are normal releases in this repository, so GitHub's **Latest** link works.
The highest successfully published build number becomes Latest, even when
builds finish out of order. Publication is queued; new main pushes do not cancel
earlier builds.

`celld --version` reports the full fork version. Local builds report
`<upstream-version>-pycelld.dev`. The manifest records both the fork and upstream
versions, the build number, and exact source commits. Published releases and
tags are never overwritten. Reruns verify existing assets; interrupted draft
uploads can resume. A failed check prevents publication of an incomplete release.

The release includes four `celld-<target>.gz` files, `SHA256SUMS`,
`BUILD_INFO.json`, `celld.pyi` for editor types, and the license. The manifest also
records target, Rust version, build profile, and checksums of compressed and
uncompressed binaries.
Download the target for your machine and `SHA256SUMS`, then for example:

```sh
# Linux x86-64; substitute the target for your platform.
grep '  celld-x86_64-unknown-linux-gnu.gz$' SHA256SUMS | sha256sum --check
gzip -dc celld-x86_64-unknown-linux-gnu.gz > celld
chmod +x celld
./celld --version
```

On macOS, use `shasum -a 256 --check` for verification. These binaries are
not signed or notarized by Apple. The binary includes the native Monty runtime;
see the [Python guide](python.md) for handlers and durable objects.
