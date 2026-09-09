# Python example

Build with `cargo xtask build`, then run:

```sh
./target/lab/celld dev examples/monty
```

[worker.py](monty/worker.py) demonstrates POST handlers, dataclasses, direct
class-based durable calls, and alarms. See the [Python API](../docs/python.md)
for the complete typed context interface.

[http](http) demonstrates `@http` routing, binary request bodies and a JSON webhook
alongside public-function endpoints. See the [HTTP API](../docs/http.md).

[extended-host](extended-host) demonstrates importing `pycelld` from Rust,
mounting Rust callbacks at `acme.native` and Python classes at `acme.greeters`,
and exporting a worker package with handlers and durable classes in submodules.
See [extending Monty](../docs/extensions.md) for build and type-check commands.
