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
stateless and durable executions. Maintainer commands also verify that every
included host source matches the pinned upstream plus the patch series.

The consumer check builds a separate application using a Git dependency on the
current commit, with no patch or preparation step in that application. It checks
generated declarations with strict mypy (including a negative type test), then
exercises custom Rust functions and Python classes over HTTP, async context
calls, dataclass RPC, and durable storage. Use `consumer.py --path` to check
uncommitted local changes before committing; CI always checks the Git import.

Native pool regressions exercise concurrent cell placement at densities 1, 2,
and 32, packing after eviction, suspension across retirement, reclaiming drained
workers, and distinct worker identities across pools and reused slots. These
run real Monty workers without initializing V8. They do not measure RSS savings
or exercise fleet rebalancing against a remote object store.

The release tests cover numeric versioning, concurrent build completion order,
idempotent reruns, draft recovery, and refusal to replace mismatched tags or assets.

The end-to-end test starts local celld processes and checks real HTTP responses,
POST routing, dataclasses, buffered bytes, native fetch, durable calls,
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
