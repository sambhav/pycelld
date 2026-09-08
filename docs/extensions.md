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

## Import paths and native Rust functions

```rust
use pycelld::{ExcType, Monty, PythonError, PythonModule, PythonValue};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let text = PythonModule::new("acme.text").with_function(
        "def uppercase(text: str) -> str: ...",
        |args| match args.as_slice() {
            [PythonValue::String(text)] => Ok(PythonValue::String(text.to_uppercase())),
            _ => Err(PythonError::new(ExcType::TypeError, Some("expected text".into()))),
        },
    )?;
    pycelld::run(Monty::new().with_module(text)?)?;
    Ok(())
}
```

Workers can then use:

```python
from acme.text import uppercase

def hello(name: str = "world") -> str:
    return uppercase("Hello, " + name)
```

Each `PythonModule` has its own namespace. Dotted paths create parent packages;
`import acme.text`, `import acme.text as text`, and
`from acme.text import uppercase as upper` all work. Modules can import each
other with absolute or relative paths, regardless of registration order.
Different modules may export functions or classes with the same name.
Duplicate module paths and replacements of Monty's standard modules are rejected.

The existing `Monty::with_function` and `Monty::with_python` methods remain
shortcuts for additions to `celld`. Use `PythonModule` for your own paths,
including `celld.my_extension` when appropriate.

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
let helpers = PythonModule::new("acme.greeters").with_python(
    include_str!("helpers.py"),
    include_str!("helpers.pyi"),
)?;
let runtime = runtime.with_module(helpers)?;
```

Both files must declare the same public function and class names. The source
can contain imports, private helpers, ordinary functions, async functions, and
classes supported by Monty. Extensions can import built-in `celld` types and
other registered modules. Multiple additions to one module share its namespace;
private helper names should be distinct within that module. Different modules
have isolated globals. Every imported module initializes once per invocation;
unimported modules do not run.

Pass `ctx: Context` explicitly to helper classes and store it in `self._ctx`.
Their methods can call registered Rust functions and use normal typed context
capabilities, including async fetch/sleep and durable storage. The
[complete example](../examples/extended-host) adds a Rust `shout` function and
Python `Greeter` and `Greeting` classes, then uses them from a handler and a
durable object.

The worker's entry module or package defines its HTTP endpoints, including
re-exports of functions from its own submodules. Imported host functions never
become endpoints. Injected classes are ordinary helpers or value classes; worker durable classes
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
target/lab/examples/extended-host types target/python-types
MYPYPATH=target/python-types mypy --strict examples/extended-host/worker examples/extended-host/helpers.py
target/lab/examples/extended-host dev examples/extended-host
```

`types DIRECTORY` writes the complete import tree: `celld.pyi`,
`acme/__init__.pyi`, `acme/native.pyi`, and `acme/greeters.pyi` in this example.
Modules with children use `__init__.pyi`. Set `MYPYPATH` to that directory and
configure your editor's stub path similarly. `types` without a directory keeps
its stdout behavior for hosts that only expose `celld`; hosts with multiple
modules require a directory. Rust embedders can use `Runtime::type_files()`.
Keep helper `.pyi` declarations beside their implementation and type check both
helpers and workers against the generated tree.

Rust callbacks run synchronously on a worker thread and cannot be preempted by
Monty's interpreter limits. Keep them bounded and nonblocking. Use Python helpers
with `ctx` for I/O so celld retains cancellation, storage scope, and durability
gates. Callback argument/result payloads are limited to 1 MiB and calls share the
existing 10,000-call invocation budget. Extension source and declarations each
have a 256 KiB budget per module. The host supports up to 256 registered modules
and 2 MiB of combined extension source and declarations. Worker package budgets
are separate; see [Python packages and import limits](python.md#packages).

The facade enables upstream's jemalloc allocator and memory-pressure accounting
by default. Applications supplying their own global allocator can use
`default-features = false`; choosing a different allocator also changes the
meaning of upstream's jemalloc statistics.

For a runtime implementation independent of Monty, use the re-exported
`pycelld::runtime::Runtime` contract with the same `run` entry point. Lower-level
host APIs are available through `pycelld::celld`; they follow the pinned host
version and are not a stable upstream extension API.

## Outbound HTTP middleware

`Monty::new()` and the supplied `celld` binary deny all Python outbound HTTP.
Workers retain the typed `await ctx.fetch(...)` interface, but cannot grant
themselves network access. An embedding Rust host installs the policy:

```rust
use pycelld::{FetchDecision, Monty};

let runtime = Monty::new().with_fetch_middleware(|ctx, mut request| {
    if request.url.scheme() != "https"
        || request.url.host_str() != Some("api.example.com")
        || request.url.port_or_known_default() != Some(443)
    {
        return FetchDecision::Deny("destination is not allowed".into());
    }
    // Optional tenant-specific policy from ctx.env and ctx.object.
    request.headers.push(("x-client".into(), "pycelld".into()));
    FetchDecision::Forward(request)
});
```

`FetchRequest` contains a parsed `url::Url`, method, header pairs and native body
bytes. `FetchContext` contains the originating request URL, environment, optional
`(class, id)` durable identity and alarm flag. Context comes from the invocation,
not from the arguments passed to `fetch`.

Register multiple callbacks to form a chain. They run in registration order;
`Forward(request)` passes any edits to the next callback, then to celld.
`Deny(message)` raises a catchable Python `RuntimeError`.
`Respond(pycelld::runtime::Response { status, headers, body })` supplies a response
without making a network call. Deny and Respond stop the chain. An injected
Python helper using `ctx.fetch` passes through the same policy as worker code,
including durable methods and alarms. Middleware is shared across compiled
programs and worker slots; callbacks must be `Send + Sync + 'static`, bounded,
and nonblocking. Rust callbacks are trusted host code: independently opening a
socket in a native extension is outside this fetch policy.

Only absolute HTTP(S) URLs without embedded credentials can be forwarded.
Request URLs and body limits are checked before each callback and after the
last rewrite. Request and response bodies remain limited to 1 MiB. Middleware
can inspect or replace a request or supply a response; it does not currently
wrap an asynchronous transport response.

Approved network I/O still goes through celld's egress configuration,
cancellation, timeouts, response-size limits and durability gates. Redirects
are **not automatically followed**: Python receives the 3xx response. A new
`ctx.fetch` to its Location runs the full policy again. Hostname rules are URL
policy, not IP-level network isolation: use controlled destinations or network
controls when address resolution must be restricted.

Deployments made with this version require `monty-filesystem-v1`, preventing
older hosts from running new deployments without filesystem and network-policy support.
Upgrade every serving host and deploy tool. Existing single-file and package
artifacts still load, now under the configured host policy. TypeScript's egress
policy is unchanged.
