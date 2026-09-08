# Validation

```sh
python3 -m unittest discover -s tools -p test_release.py -v
cargo xtask test
cargo xtask build
python3 tools/monty-checks/e2e.py target/lab/celld
python3 tools/monty-checks/consumer.py
python3 tools/storage-checks/probe.py target/lab/celld -v
```

`cargo xtask test` covers the standalone Monty interpreter and typed runtime
boundary, a second runtime using the public extension API, native worker
cancellation and capacity, deployment compilation, host configuration, cell
placement, error/cancellation durability positions, sync proof refusal, paged
SQLite activation, and reproducible patch preparation.

Extension regressions cover keyword/default binding, native binary values,
catchable Rust errors, registration validation, budgets, and helper classes in
stateless and durable executions. Module tests cover aliases, same-name exports,
relative and circular imports, isolated globals and closures, lazy initialization,
package source discovery, path boundaries, and qualified durable identities. Maintainer commands also verify that every
included host source matches the pinned upstream plus the patch series.

The consumer check builds a separate application using a Git dependency on the
current commit, with no patch or preparation step in that application. It checks
generated declarations with strict mypy (including a negative type test), then
exercises custom Rust functions and Python classes over HTTP, async context
calls, dataclass RPC, and durable storage from a worker package importing two
custom extension modules. Use `consumer.py --path` to check
uncommitted local changes before committing; CI always checks the Git import.
For a faster local build, add `--profile dev` to the consumer command and
`cargo xtask test`; CI uses the optimized `lab` profile.

Native pool regressions exercise concurrent cell placement at densities 1, 2,
and 32, packing after eviction, suspension across retirement, reclaiming drained
workers, and distinct worker identities across pools and reused slots. These
run real Monty workers without initializing V8. They do not measure RSS savings
or exercise fleet rebalancing against a remote object store.

The release tests cover numeric versioning, concurrent build completion order,
idempotent reruns, draft recovery, and refusal to replace mismatched tags or assets.

The end-to-end test starts local celld processes and checks real HTTP responses,
POST routing, dataclasses, buffered bytes, default network denial, durable calls,
concurrency, transactions, SQL, alarms, cancellation, and disk recovery. Storage
compatibility checks exercise conditional writes against strict local S3
fixtures.

Check the editor interface independently:

```sh
python3 -m pip install mypy==2.3.1
MYPYPATH=crates/monty-runtime/src mypy --strict examples/monty/worker.py
MYPYPATH=crates/monty-runtime/src mypy --strict tools/monty-checks/bench/worker.py
```

For a lightweight comparison against TypeScript workers:

```sh
python3 tools/monty-checks/bench.py target/lab/celld \
  --seconds 1 --repeats 3 --concurrency 1 16 \
  --output tools/monty-checks/bench/results-native.json
```

Every benchmark response is validated; errors abort the run. Both runtimes use
the same binary, local HTTP, one stateless slot, and identical workloads.
See the [benchmark report](../tools/monty-checks/bench/README.md) for results and
measurement limitations. Streaming and remote iterators remain outside scope.


Runtime extension tests cover fetch middleware ordering, URL rewriting, denial,
synthetic binary responses, request limits, and durable/helper calls. Native
host integration tests exercise a real loopback 302 response and verify its
target receives no connection. The binary end-to-end suite verifies that the
stock runtime denies fetch.

## Durable filesystem

Runtime tests exercise Monty's existing OS-call suspension, native binary
buffers, text write counts and catchable filesystem/UTF-8 errors. Storage tests
cover hierarchy, subtree rename, append, path and quota bounds, atomic failures
and rollback. The native worker test checks SQL table protection, file write
positions after a raised handler, ownership reactivation, and a real sparse
restore through celld's paged VFS using an in-memory page source.

The HTTP test uses `Path` and `open()` from Python durable objects, checks
isolation and mixed file/key-value rollback, syncs writes, restarts the process
and reads the files back. It also checks `storage.clear()` and denied stateless
access. These tests exercise the normal database path; they do not contact S3.
