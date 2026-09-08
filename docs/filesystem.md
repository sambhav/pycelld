# Durable files

Use `pathlib.Path` and `open()` inside a durable object. `/` is that object's
private root; relative paths start there too. Files and folders live in the
object's SQLite database and return when celld restores the object. No Monty
fork, host directory mount, or JavaScript adapter is involved.

```python
from pathlib import Path
from celld import Context

class Notebook:
    def __init__(self, id: str, ctx: Context):
        self.id = id
        self._ctx = ctx

    def save(self, text: str) -> None:
        with self._ctx.storage.transaction():
            Path("notes").mkdir(parents=True, exist_ok=True)
            Path("notes/latest.txt").write_text(text)
            self._ctx.storage.set("last_file", "notes/latest.txt")

    def read(self) -> str:
        return Path("notes/latest.txt").read_text()

    def append(self, text: str) -> None:
        with open("notes/latest.txt", "a") as file:
            file.write(text)

def save(ctx: Context, id: str, text: str) -> None:
    Notebook(id, ctx).save(text)
```

Supported operations: text and binary reads/writes, append, `mkdir`, `iterdir`,
`exists`, `is_file`, `is_dir`, `stat`, `rename`, `unlink`, `rmdir`, `resolve` and
`absolute`. `is_symlink` returns false. File modes are `r`, `rb`, `w`, `wb`, `a`
and `ab`, as supported by the pinned Monty version. Text is UTF-8.

Missing files, existing destinations, invalid parent directories and denied
paths raise Python filesystem exceptions. Stateless handlers receive
`PermissionError`; call a durable object to select the owner. The Python caller
cannot select another object's database through a filename.

## Persistence and transactions

Every operation uses the active object SQLite connection, including celld's
paged VFS. Files are BLOBs in a reserved `_cf_` table protected from user SQL.
File operations join `ctx.storage.transaction()`, including nested rollback.
A single rename, append or other operation is atomic. Outside a transaction,
a later Python exception does not roll back earlier writes.

The existing output and external-I/O durability gates cover file writes.
`ctx.storage.sync()` waits for their durability too. `ctx.storage.clear()`
removes files along with the object's other storage. The root is recreated on
the next file operation. First access initializes the filesystem table within
the same storage transaction.

Celld's normal database recovery restores contents and folder structure; no
separate snapshot/export service is required. Open handles and interpreter state
are invocation-local. Monty buffers file reads and identifies open files by path;
do not depend on POSIX handle behavior across rename or unlink. Writes are
applied when the file method runs, rather than deferred until close.

## Bounds

- 1 MiB per file, 16 MiB total file content per object.
- 1,024 entries including the root, 32 path components, 4,096 UTF-8 bytes per path.
- Directory listings are sorted and limited by the entry count and 1 MiB reply budget.
- File operations count toward the invocation's existing 10,000 host-call limit.

Paths use a virtual POSIX namespace. `.` and `..` are normalized; traversal above
root and NUL bytes are rejected. Directory moves validate every descendant's
limits before committing. This bounded implementation stores paths directly;
a directory move updates at most 1,024 rows inside one savepoint.

There are no host mounts, symlinks, devices, sockets, Unix permission management,
cross-object moves or large-file streaming. `stat` reports file type, size and
modification time; synthetic permission bits do not grant host access.
