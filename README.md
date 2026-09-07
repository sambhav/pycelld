# Monty workers for celld

Write Python handlers and durable objects on [celld](https://github.com/denoland/celld),
with [Monty](https://github.com/pydantic/monty) executing entirely in Rust.

This repository maintains two Rust crates and three patches against a pinned
celld release. `cargo xtask build` fetches upstream, applies the patches, and
links Monty into the ordinary celld binary. An upstream source copy or hosted
fork is not needed in this repository.

## Build and run

[Download celld binaries](https://github.com/sambhav/pycelld/releases) for Linux
and macOS, on x86-64 and ARM64. Each `main` build publishes a tested prerelease
with checksums and Python editor types. See [installation](docs/fork.md#download-and-release-binaries).

Use Git and Rust 1.98.1. TypeScript bundling also requires `esbuild` on `PATH`;
Python deployment does not invoke it.

```sh
cargo xtask build
./target/lab/celld dev examples/monty
```

The default build uses celld's optimized `lab` profile. For a release binary:

```sh
cargo xtask build --release
./target/release/celld types > celld.pyi
```

## Python interface

Public functions become POST handlers. Context is optional:

```python
from dataclasses import dataclass
from celld import Context

@dataclass
class Greeting:
    message: str

def hello(name: str = "world") -> Greeting:
    return Greeting(f"Hello, {name}!")

class Counter:
    def __init__(self, id: str, ctx: Context):
        self.id = id
        self._ctx = ctx

    def increment(self) -> int:
        value = self._ctx.storage.get("count", 0)
        assert isinstance(value, int)
        self._ctx.storage.set("count", value + 1)
        return value + 1

def increment(ctx: Context, id: str) -> int:
    return Counter(id, ctx).increment()
```

Send `POST /hello` with `{"name":"Sam"}` or `POST /increment` with
`{"id":"visits"}`. Durable classes are discovered and registered automatically.
Returns map to HTTP responses, and exceptions become structured errors. Durable
calls preserve supported Python values, including dataclasses. Iterators and
streaming are deferred.

See the [Python API](docs/python.md) and [complete example](examples/monty/worker.py).

## Rust integration

- [`celld-runtime`](crates/runtime-api/src/lib.rs) defines compilation, suspended
  execution, typed host calls, and native response buffers.
- [`celld-monty`](crates/monty-runtime) implements that contract. It has no
  dependency on celld, V8, Tokio, or SQLite internals.
- [`patches`](patches) adds native host support and registers the Monty crate.
  celld retains scheduling, cell ownership, storage gates, cancellation, alarms,
  and HTTP transport. TypeScript workers keep the existing V8 path.

The integration is statically linked. There is no JavaScript bridge or separate
interpreter process. The pinned upstream revision is recorded in
[`upstream/revision`](upstream/revision); upgrading it is an explicit change.

## Development

```sh
cargo xtask prepare  # fetch and apply patches, without compiling celld
cargo xtask test     # runtime, host integration, and build-workflow tests
python3 tools/monty-checks/e2e.py target/lab/celld
```

- [Builds, patch maintenance, and the runtime boundary](docs/build.md)
- [Validation and benchmarks](docs/testing.md)
- [Host configuration and binary releases](docs/fork.md)
- [Benchmark results](tools/monty-checks/bench/README.md)

Upstream architecture and operational documentation are available in the
[celld repository](https://github.com/denoland/celld/tree/a52f9905425bc41134d817694bdc2c50bcc5e856/docs).
