# Builds and upstream patches

`cargo build --locked --release` builds the celld executable from the included
host sources. Applications can [depend on `pycelld`](extensions.md) directly.
Maintainers use `cargo xtask build` to verify the included sources against the
pinned upstream and patches before building. Only Git and Rust are needed by
the preparation tool; `xtask` has no crate dependencies.

## Reproducible inputs

| Input | Purpose |
| --- | --- |
| `upstream/repository` | Official upstream Git repository |
| `upstream/revision` | Full pinned commit SHA; currently celld 0.4.1 |
| `patches/*.patch` | Patches applied in filename order |
| `upstream/Cargo.lock` | Dependency lock for the generated celld workspace |
| `Cargo.lock` | Dependency lock for the facade, host, and runtime crates |
| `vendor/celld` | Prepared build inputs included for downstream Cargo consumers |

Preparation fetches the exact upstream commit, checks and applies every patch,
and installs the committed upstream lockfile. `cargo xtask vendor` copies its
build inputs into `vendor/celld`; `cargo xtask vendor --check` verifies the exact
file set and contents. The maintained build and test commands require this check
to pass and run Cargo with `--locked`.
Unchanged inputs reuse the prepared checkout without fetching. Dependency
availability still determines whether Cargo itself can build offline.

Preparation refuses to discard staged edits, unstaged edits, or untracked files
in the generated checkout. A failed fetch or patch leaves the previous checkout
intact. One invocation holds an operating-system file lock to prevent concurrent
replacement; exiting or being killed releases it automatically. After a killed
preparation, inspect any incomplete `target/celld-preparing` directory before
removing it and retrying.

## Commands

```sh
cargo xtask prepare
cargo xtask vendor                      # regenerate after changing host patches
cargo xtask vendor --check              # reproducibility check
cargo xtask build                       # optimized lab build
cargo xtask build --release             # release build
cargo xtask build --release --target x86_64-unknown-linux-gnu
cargo xtask test
```

Binaries appear under the root `target` directory, for example
`target/lab/celld` or `target/x86_64-unknown-linux-gnu/release/celld`.
`cargo test -p celld-monty -p celld-runtime --locked` checks the standalone crates
without building celld. `cargo xtask test` additionally checks native host integration and the
build utility. Run the HTTP end-to-end suite separately as described in
[testing](testing.md).

## Runtime boundary

The pin is celld [0.4.1](https://github.com/denoland/celld/releases/tag/v0.4.1).
The native bridge carries response write/observed positions, gates sync against
committed state at its activation epoch, honors paged SQLite restores, and
retains upstream storage failure tracking. Monty's Python API remains unchanged;
upstream's new embedded facets are not exposed to Python yet.

The API follows the current bounded execution model:

1. `Runtime` recognizes entry paths, bundles source, compiles an artifact, and
   supplies editor type files. Default hooks preserve single-file runtimes.
2. `Program` caches compilation, forks once per worker slot, and starts calls.
3. `Execution::resume` consumes a typed host reply. It returns either a completed
   response or the next typed `HostCall`.

Programs and suspended executions are `Send`, with no concurrent entry into an
individual instance. Language execution must bound its interpreter CPU work.
Dropping an execution cancels its interpreter state; celld independently owns
I/O cancellation, transaction rollback, authority, and gate cleanup. Storage
values are JSON; fetch and durable-call bodies cross the interface as byte
buffers. Serialization of remote Python values belongs to Monty.

Monty bundles package sources into a deterministic artifact. New deployments
require `monty-http-v1`; the runtime also advertises
`monty-native-v1`, `monty-modules-v1`, `monty-filesystem-v1`,
`monty-network-policy-v1`, `monty-execution-v1` and `monty-observability-v1`, and continues loading older single-file
and package artifacts under the configured network policy. Older hosts reject
new deployments during feature negotiation.

The host provides a scoped context internally. Runtime code cannot select a
storage scope, access a SQLite connection, or bypass an output gate. Python's
fully typed `Context` remains part of the Monty crate.

The patches are deliberately separate:

- `0001-host-options.patch`: existing cell-density and S3 ETag options.
- `0002-native-runtime.patch`: the language-independent native host bridge,
  deployment hooks, feature negotiation, and public runtime registration.
- `0003-link-monty.patch`: optional crate dependency, integration-test target,
  owned runtime registration at startup, the callable `celld::command::run()`
  entry point, and the distribution version reported by `celld --version`.

- `0004-durable-filesystem.patch`: scoped SQLite filesystem operations through
  the language-independent native contract.

- `0005-execution-context.patch`: host identities, native ceilings, and trusted
  nested-call correlation across local dispatch and peer tunnels.
- `0006-python-observability.patch`: invocation-scoped Python diagnostics,
  structured OTLP logs and custom spans.

Release builds set `CELLD_RELEASE_VERSION` to the version generated by CI.
Local builds use `<upstream-version>-pycelld.dev`. This only changes the CLI's
distribution label; Cargo manifests and the dependency lock retain upstream's
package version. See [release versioning](fork.md#download-and-release-binaries).

The registration patch moves celld's existing CLI entry into its library, with
a small binary entry point preserving the allocator and startup ordering.
The facade registers the configured runtime before entering that CLI. A second tiny
test runtime exercises registration, feature negotiation, async completion, and
cancellation without importing Monty.

## Updating the integration

Normal Python/runtime changes belong in `crates/monty-runtime` and need no host
patch change. Changes to the public contract belong in `crates/runtime-api`.
Do not edit `vendor/celld` directly; it is a generated copy, including upstream
licenses, for ordinary Cargo dependency resolution.

For a host change, prepare the checkout and edit `target/celld`. Its Git index
records the fully patched baseline, so `git diff` contains only your new edits:

```sh
git -C target/celld diff --binary > patches/0004-description.patch
```

Use `git add -N <path>` inside that checkout for a new file before exporting its
diff. Review and retain the patch before restoring the generated checkout's
edits; the next preparation will then apply the additional patch. To keep the
series small, fold a reviewed follow-up into the relevant existing patch when
updating upstream.

To upgrade celld, change `upstream/revision`, rebase the patches against that
commit, and update `upstream/Cargo.lock` and the root lock when dependencies
change. Regenerate `vendor/celld` and commit it alongside the pin and patches.
Run the full validation suite, including the downstream Git dependency check,
and compare benchmarks. The current native bridge is an
internal refactor carried as a patch; its public interface is small, but it is
not an upstream-supported plugin API yet.
