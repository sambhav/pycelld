# Python workers

Point `main` at a Python file or package directory containing `__init__.py`. Monty is built into celld; no Python installation,
SDK, bundler or runtime setting is needed.

Custom hosts can [add typed Python functions, classes, and Rust callbacks](extensions.md)
at custom import paths using the `pycelld` Rust crate.

`wrangler.jsonc`:

```json
{"name": "hello", "main": "worker.py"}
```

`worker.py`:

```python
from dataclasses import dataclass

@dataclass
class Greeting:
    message: str

def hello(name: str = "world") -> Greeting:
    return Greeting(f"Hello, {name}!")
```

```sh
celld dev
curl http://localhost:9876/hello -d '{"name":"Ada"}'
# {"message":"Hello, Ada!"}
```

Every public function declared in the entry file becomes `POST /function`.
A JSON object supplies named arguments; defaults and keyword-only parameters
work normally. Declare `ctx: Context` to receive host capabilities. Callers
cannot supply `ctx`. Empty bodies mean `{}`. A literal `__all__` can restrict
exports. Functions re-exported from project modules are supported; imported host
functions and private names are never handlers.

For arbitrary methods, nested paths, webhooks and binary bodies, export an
[explicit HTTP handler](http.md) with `@http`. Public-function endpoints can coexist.

## Packages

A worker can export a whole package:

```json
{"name": "shop", "main": "shop"}
```

`shop/__init__.py` defines its public API:

```python
from .handlers import hello, increment

__all__ = ["hello", "increment"]
```

Put implementations in `shop/handlers.py`, durable classes in `shop/objects.py`,
and shared value classes or helpers in other submodules. Normal absolute,
relative, and aliased imports work. The functions above become `POST /hello`
and `POST /increment`; submodule paths are not URL prefixes. Without `__all__`,
public functions defined or re-exported by the entry module become handlers.
`__all__` must be one literal list or tuple of public names.

A directory entry includes every `.py` file beneath it; `main` may also name
its `__init__.py`. A file entry includes its transitive local imports and parent
package initializers. Imported source must stay inside the project. Changes to
included source change the deployment version. Data files are not bundled;
source symlinks within package directories and ambiguous module/package names
are rejected.

Modules have isolated globals and initialize lazily, once per invocation.
Circular imports work when initialization does not read a name before the other
module defines it. Module namespaces and function values stay inside Monty. Python parsing and binding resolution happen in Rust; executing an import
never reads the host filesystem or calls JavaScript. No Monty fork is required.

This supports source packages within Monty's Python subset, not installation of
wheels or CPython extensions. Static named imports and `from __future__ import
annotations` are supported. Wildcard imports, dynamic module discovery
(`importlib`, `sys.modules`), module/global assignment expressions (`:=`), and
module/global exception aliases (`except ... as name`) are unsupported. Catch
exceptions in a function when an alias is needed.

## Responses

Return a value or raise an exception. Use `Response` for explicit status and
headers.

| Return | HTTP response |
| --- | --- |
| `str` | 200, UTF-8 text |
| `bytes` | 200, binary |
| `None` | 204, empty body |
| Number, boolean, container or dataclass | 200, JSON value |
| `Response(body, status=201, headers={...})` | Explicit response; body is `str` or `bytes` |
| Invalid arguments | 400, structured error |
| Uncaught exception | 500, `{"error":{"code":"ValueError","message":"..."}}` |

JSON conversion is recursive. Tuples and sets become arrays, named tuples become
objects, dates/times/paths become strings, and timedeltas become seconds.
Nested bytes become arrays of byte values. Dictionary keys must be strings;
non-finite numbers, cycles and unsupported values fail instead of being
silently stringified. Iterators are rejected without consumption. Streaming
and NDJSON are deferred; return a list for a buffered JSON array.

## Durable objects

Define a class with `__init__(self, id: str, ctx: Context)`. Construct it directly
and call its ordinary methods:

```python
from celld import Context

class Counter:
    def __init__(self, id: str, ctx: Context):
        self.id = id
        self._ctx = ctx

    def increment(self, amount: int = 1) -> int:
        value = self._ctx.storage.get("count", 0)
        assert isinstance(value, int)
        value += amount
        self._ctx.storage.set("count", value)
        return value

def increment(ctx: Context, id: str, amount: int = 1) -> int:
    return Counter(id, ctx).increment(amount)
```

`Counter(id, ctx)` creates a typed handle. Its public methods run on the owning
node, where the real constructor receives the object's context. Store that
context privately. Celld discovers classes and registers SQLite storage;
no decorators, bindings, migrations or generated clients are needed.
Ordinary helper classes and dataclasses remain local.

Use a nonempty string ID, up to 1,024 characters. Routing happens before the
constructor runs, so compose IDs at the call site: `Counter(f"{tenant}/{cart}", ctx)`.
The class and ID identify persistent storage. Package classes use the defining
module path, such as `shop.objects.Counter`; moving that definition changes its
storage identity. Re-exporting it under another name does not. File-entry classes
retain their existing bare identity, such as `Counter`, for compatibility.
IDs are not authorization boundaries.

Each method is a serialized turn, including awaited I/O. Call async methods
with `await`; call the current object's methods through `self`. Avoid cyclic
calls between objects. `ctx.storage` persists across reload and restart;
globals and instance fields reset on each invocation. Returned dataclasses carry
field values, not remote methods.

## Context and types

```sh
celld types > celld.pyi
```

This writes [the built-in module's types](../crates/monty-runtime/src/celld.pyi)
next to your worker for editors and type checkers. Import `Context`, `Response`,
`Request`, `Storage`, `Alarms`, `Json` and `SqlValue` from `celld`. All public
members are typed without `Any`.

| API | Behavior |
| --- | --- |
| `ctx.request.method`, `.url`, `.headers` | Incoming request metadata |
| `ctx.env` | Configured `dict[str, str]` variables |
| `ctx.id` | Durable ID, or `None` in a stateless handler |
| `ctx.storage.get(key, default=None)` | Read a JSON value; default applies only to missing keys |
| `ctx.storage.set(key, value)` | Persist a JSON value |
| `ctx.storage.delete(key)` | Delete; return whether the key existed |
| `ctx.storage.list(prefix="", limit=1000, reverse=False)` | Dictionary of matching values |
| `ctx.storage.clear()` | Remove all keys |
| `ctx.storage.sql(query, *bindings)` | Parameterized SQL returning row dictionaries |
| `ctx.storage.transaction()` | Context manager; commit on success, roll back on exception |
| `ctx.storage.sync()` | Wait for preceding operations to pass the durability gate |
| `ctx.alarms.set(when)` | Schedule `alarm(self)` using a `timedelta` or aware `datetime` |
| `ctx.alarms.get()`, `.delete()` | Inspect or remove the alarm |
| `await ctx.fetch(url, method="GET", headers=..., body=...)` | Requires host fetch middleware; returns a typed `Response`, including redirects |
| `await ctx.sleep(seconds)` | Cancellable sleep, up to 30 seconds |
| `ctx.now()`, `.uuid()`, `.log(message)` | UTC datetime, random UUID string, and logging |

Storage and alarms require a durable object. Storage values use the recursive
`Json` type; narrow reads with `isinstance`. Use a transaction for rollback:

```python
with self._ctx.storage.transaction() as storage:
    storage.set("count", 1)
    storage.sql("INSERT INTO events (message) VALUES (?)", "incremented")
```

Nested transactions use savepoints. External I/O and object calls are rejected
inside transactions. An exception outside a transaction does not undo earlier
writes.

## Runtime and limits

Python executes entirely in Rust, including HTTP, async operations, durable
calls, storage and alarms. It shares celld's admission, ownership, cancellation
and durability mechanisms. Python workers allocate no V8 isolate; the binary
still includes V8 for TypeScript. See the [benchmark](../tools/monty-checks/bench/README.md)
and [test instructions](testing.md).

Limits: 256 KiB per source module, 256 worker modules and 1 MiB total worker
source per deployment, 32 package directory levels, 1 MiB per request/result/host
reply, 100 ms of
interpreter execution, 10,000 host operations, 256 live invocations per worker
slot, 16 nested transactions, and a 30-second wall-clock deadline per durable turn.

Monty implements a reduced Python subset. External package installation, native
Python extensions, `yield`, async iterators, streaming, WebSockets, queues and
workflows are unsupported.


Outbound HTTP is disabled in the supplied binary. Custom Rust hosts can install
[fetch middleware](extensions.md#outbound-http-middleware) to inspect, rewrite,
answer or deny each `ctx.fetch` call. A denied request raises `RuntimeError`.
Redirects are returned without following them.

Use [`pathlib` and `open()`](filesystem.md) inside durable objects for persistent
files and folders. File writes share storage transactions, durability gates and
database recovery. Each object has a private filesystem root.
