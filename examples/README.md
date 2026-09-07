# Python example

Build with `cargo xtask build`, then run:

```sh
./target/lab/celld dev examples/monty
```

[worker.py](monty/worker.py) demonstrates POST handlers, dataclasses, direct
class-based durable calls, and alarms. See the [Python API](../docs/python.md)
for the complete typed context interface.

[extended-host](extended-host) demonstrates importing `pycelld` from Rust,
injecting native functions and Python classes, and running the patched host.
See [extending Monty](../docs/extensions.md) for build and type-check commands.
