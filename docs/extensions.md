# Extend Monty from your Rust application

Import `pycelld` to get the patched celld host, native Monty runtime, and typed
runtime contract together. This is a Git dependency, not a crates.io package:

```toml
[dependencies]
pycelld = { git = "https://github.com/sambhav/pycelld" }
```

Commit your application's `Cargo.lock`; use `rev` or a release `tag` to pin a
specific pycelld revision. Build with Rust 1.98.1. Cargo uses the included host
sources directly. Your application does not run `xtask` or repeat `[patch]`
sections. Cargo only reads patch settings from the consuming workspace's root;
see the [Cargo reference](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html#the-patch-section).

## Native Rust functions

```rust
use pycelld::{ExcType, Monty, PythonError, PythonValue};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Monty::new().with_function(
        "def uppercase(text: str) -> str: ...",
        |args| match args.as_slice() {
            [PythonValue::String(text)] => Ok(PythonValue::String(text.to_uppercase())),
            _ => Err(PythonError::new(ExcType::TypeError, Some("expected text".into()))),
        },
    )?;
    pycelld::run(runtime)?;
    Ok(())
}
```

Workers can then use:

```python
from celld import uppercase

def hello(name: str = "world") -> str:
    return uppercase("Hello, " + name)
```

The signature must be a synchronous Python `def` with annotations on every
parameter and the return value, and an ellipsis body. Defaults, positional-only
parameters, and keyword-only parameters work; variadic parameters and decorators
are unsupported. Python binds the call and Rust receives owned `PythonValue`s in
declaration order. Validate their types in the callback: annotations support
editors and static checks, and do not enforce runtime types.

Values are Monty's native values, including bytes, containers, and supported
dataclass values. They cross into Rust without JSON serialization. Return
`PythonError` to raise a catchable Python exception. Closures can capture shared
Rust state and must be `Send + Sync + 'static`; calls from different workers can
run concurrently.

## Python functions and classes

Register Python source alongside its public `.pyi` declarations:

```rust
let runtime = runtime.with_python(
    include_str!("helpers.py"),
    include_str!("helpers.pyi"),
)?;
```

Both files must declare the same public function and class names. The source
can contain imports, private helpers, ordinary functions, async functions, and
classes supported by Monty. Extensions can import built-in `celld` types and
previously registered extensions. Duplicate public names and replacements of
built-in context types are rejected. These files share the worker's Python
namespace, so use distinct helper names across extensions.

Pass `ctx: Context` explicitly to helper classes and store it in `self._ctx`.
Their methods can call registered Rust functions and use normal typed context
capabilities, including async fetch/sleep and durable storage. The
[complete example](../examples/extended-host) adds a Rust `shout` function and
Python `Greeter` and `Greeting` classes, then uses them from a handler and a
durable object.

Only public functions declared in the worker entry file become HTTP endpoints.
Injected classes are ordinary helpers or value classes; worker durable classes
still use the [durable constructor contract](python.md). Returned values and
raised exceptions follow the existing response rules. Streaming remains deferred.

## Run and type check

`pycelld::run(runtime)` registers the configured runtime once and runs celld's
ordinary CLI. Use it from `main`, before starting any async executor. Your binary
supports `dev`, `deploy`, `types`, and production serving. Deployment tooling and
every server running the worker must use the same extension configuration.
Extensions are compiled into your binary; installing a worker does not install
new host functions.

Run the included example:

```sh
cargo build --locked --profile lab --example extended-host
target/lab/examples/extended-host types > celld.pyi
MYPYPATH=. mypy --strict examples/extended-host/worker.py examples/extended-host/helpers.py
target/lab/examples/extended-host dev examples/extended-host
```

`types` includes the complete built-in context declarations, native function
signatures, and Python extension stubs. Keep the stubs beside their implementation
and type check both helpers and workers against the generated file.

Rust callbacks run synchronously on a worker thread and cannot be preempted by
Monty's interpreter limits. Keep them bounded and nonblocking. Use Python helpers
with `ctx` for I/O so celld retains cancellation, storage scope, and durability
gates. Callback argument/result payloads are limited to 1 MiB and calls share the
existing 10,000-call invocation budget. Extension source and declarations each
have a 256 KiB budget in addition to the worker's source budget.

The facade enables upstream's jemalloc allocator and memory-pressure accounting
by default. Applications supplying their own global allocator can use
`default-features = false`; choosing a different allocator also changes the
meaning of upstream's jemalloc statistics.

For a runtime implementation independent of Monty, use the re-exported
`pycelld::runtime::Runtime` contract with the same `run` entry point. Lower-level
host APIs are available through `pycelld::celld`; they follow the pinned host
version and are not a stable upstream extension API.
