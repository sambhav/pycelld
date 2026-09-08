# Durable filesystem assessment

Status: design only. pycelld currently rejects filesystem OS calls; no filesystem
is mounted, persisted or restored by this change.

## Feasibility

Yes: ordinary file and folder operations can use the same SQLite database and
durability machinery as a durable object's existing storage. No Monty fork is
needed. The pinned Monty revision already exposes typed
[`OsFunctionCall`](https://github.com/pydantic/monty/blob/af272c3116e2525249103f960b79086fd250bcef/crates/monty-types/src/os.rs)
values and a resumable
[`OsCall`](https://github.com/pydantic/monty/blob/af272c3116e2525249103f960b79086fd250bcef/crates/monty/src/run_progress.rs).
`pathlib.Path` and `open()` suspend the interpreter; the Rust host decides what
those operations mean. File writes yield typed write/append operations, so they
need not wait for a whole interpreter snapshot to be saved.

Monty's [`monty-fs`](https://github.com/pydantic/monty/tree/af272c3116e2525249103f960b79086fd250bcef/crates/monty-fs)
implements host directory mounts and an in-memory overlay. Those are useful for
local mounts but do not automatically participate in celld's replicated SQLite
state. Serializing an overlay after every request would add whole-tree copying
and a second commit boundary. A SQLite-backed virtual filesystem fits celld
better.

## Proposed Python interface

Inside a durable object, `/` is that object's private virtual root. Relative
paths start there on each invocation. The constructor and context API stay the
same. For example, after implementing the filesystem:

```python
from pathlib import Path
from celld import Context

class Notebook:
    def __init__(self, id: str, ctx: Context):
        self.id = id
        self._ctx = ctx

    def save(self, text: str) -> None:
        folder = Path("notes")
        folder.mkdir(parents=True, exist_ok=True)
        (folder / "latest.txt").write_text(text)

    def read(self) -> str:
        return Path("notes/latest.txt").read_text()
```

`open("notes/latest.txt", "w")` should use the same backend. The initial scope
should cover files, directories, text/bytes, append, listing, stat, rename and
delete. Start without symlinks, host mounts, devices, sockets, Unix permissions
or cross-object moves. Do not promise full POSIX semantics or unsupported Monty
file modes. Persistent access from stateless handlers should raise a clear error;
call a durable object to select the filesystem owner. An optional ephemeral
scratch mount can be a separate follow-up.

## Rust and durability boundary

1. Extend pycelld's Monty session to retain and resume `RunProgress::OsCall`,
   beside the existing external-function suspension. Keep Monty-specific types
   inside the Monty runtime crate.
2. Translate supported operations into a small typed filesystem contract in
   `celld-runtime`. The native celld host supplies the current object scope;
   Python must never select another object's database through a path.
3. Store directories and file contents in reserved SQLite tables. An inode
   table with `(parent, name)` uniqueness permits directory rename without
   rewriting every descendant path. Use BLOB contents, not JSON byte arrays.
4. Run each mutation atomically under the existing object gates and SQLite
   transactions. File operations inside `ctx.storage.transaction()` should join
   that transaction, allowing file and key/value changes to commit together.
   Exceptions outside an explicit transaction should follow existing storage
   semantics; do not imply automatic rollback of the entire invocation.
5. Let existing output/egress durability gates govern acknowledgement and
   `ctx.storage.sync()` cover filesystem writes too. Files become ordinary
   replicated database pages. Celld's existing LTX/node-log/bucket restoration,
   including 0.4.1's packed storage path, then restores the hierarchy and data.

There is no separate directory export/import on restart. Object activation
restores the database through celld's normal mechanism; reads resolve from its
filesystem tables. Open file handles and Python execution state are not durable.

## Work and constraints

This is a contained feature, not a mount option we can simply enable. It needs
OS-call dispatch, scoped SQL-backed operations, typed Python filesystem errors,
and host integration tests. Normalize paths with virtual POSIX rules; reject
attempts to traverse above the root, enforce parent-directory and rename rules,
and never pass these paths to `std::fs`.

Set explicit per-file, per-object byte, entry-count, path-depth and directory
listing limits. Start with bounded whole-file operations suited to Monty's
current buffered file interface; large files would need a separate chunking
and resource-budget design. Maintain quotas inside the same transaction as the
write, including overwrite, append and rollback.

Validation should cover two-object isolation, nested folders, text and binary
round trips, atomic rename, transaction rollback, exception mapping, quota and
path failures, and restore after real process restart and ownership transfer.
Add a 0.4.1 packed-VFS recovery case to prove filesystem tables use the active
restored database rather than opening a separate local SQLite file.

Recommendation: implement this SQLite-backed filesystem directly against the
existing Monty OS-call API. Keep local host mounts and interpreter checkpointing
out of the initial durable filesystem feature.
