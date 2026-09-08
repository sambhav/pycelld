# Monty workers for celld

Write Python handlers and durable objects on [celld](https://github.com/denoland/celld),
with [Monty](https://github.com/pydantic/monty) executing entirely in Rust.

Use the supplied binary, or import the `pycelld` Rust crate to build a host with
your own Monty functions and classes at custom Python import paths. The crate includes the patched celld host;
applications need no patch commands or separate celld fork. Four maintained
patches reproduce the included host sources from celld 0.4.1.

## Build and run

[Download celld binaries](https://github.com/sambhav/pycelld/releases/latest) for Linux
and macOS, on x86-64 and ARM64. Linux binaries require glibc 2.28 or later. Every push to `main` automatically publishes a
tested release such as `0.4.1-pycelld.4`: upstream celld version plus an increasing
build number. Checksums and Python editor types are included. See
[installation and versioning](docs/fork.md#download-and-release-binaries).

Use Git and Rust 1.98.1. TypeScript bundling also requires `esbuild` on `PATH`;
Python deployment does not invoke it.

```sh
cargo build --locked --profile lab
./target/lab/celld dev examples/monty
```

The default build uses celld's optimized `lab` profile. For a release binary:

```sh
cargo build --locked --release
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

Durable objects can use [`pathlib.Path` and `open()`](docs/filesystem.md) for
persistent files and folders. Each object has a private root backed by its
SQLite database, sharing storage transactions and recovery.

Outbound HTTP is denied by default. Embedding hosts can install
[fetch middleware](docs/extensions.md#outbound-http-middleware) to approve,
rewrite, answer or deny calls without a JavaScript bridge.

Workers can also export a package through its `__init__.py`; set `main` to the
package directory.

See the [Python API](docs/python.md) and [complete example](examples/monty/worker.py).

## Rust integration

- [`pycelld`](src/lib.rs) includes the patched host, Monty, and the runtime API.
  Mount extensions with `Monty::new().with_module(PythonModule::new("acme.api")...)`, then call
  `pycelld::run(runtime)` from your binary. See [extending Monty](docs/extensions.md)
  and the [custom host example](examples/extended-host/main.rs).
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
cargo xtask vendor --check  # verify included host sources match the patches
cargo xtask test     # runtime, host integration, and build-workflow tests
python3 tools/monty-checks/e2e.py target/lab/celld
python3 tools/monty-checks/consumer.py  # separate Git dependency, types, HTTP
```

- [Builds, patch maintenance, and the runtime boundary](docs/build.md)
- [Validation and benchmarks](docs/testing.md)
- [Host configuration and binary releases](docs/fork.md)
- [Benchmark results](tools/monty-checks/bench/README.md)

Upstream architecture and operational documentation are available in the
[celld repository](https://github.com/denoland/celld/tree/10cb1303dac710dcb3b557e318e08c855261f68b/docs).
