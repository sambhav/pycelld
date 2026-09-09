# Python development tools

The `pycelld` Python package installs native release binaries, generates projects
and editor types, checks compatibility, and provides pytest fixtures. It has no
runtime dependencies; the optional `test` extra installs pytest. Application
code runs only inside the selected celld binary.

## Install and create a project

Use Python 3.12 or newer and Git for the initial tooling install, on Linux
(glibc 2.28+) or macOS, on x86-64 or ARM64:

```sh
python3 -m venv .venv
. .venv/bin/activate
python -m pip install 'pycelld[test] @ git+https://github.com/sambhav/pycelld.git'
pycelld install
pycelld init hello
pycelld dev hello
```

Then, in another terminal with that environment activated:

```sh
curl -X POST localhost:9876/hello -H 'Content-Type: application/json' -d '{"name":"Sam"}'
pycelld check hello
python -m pytest hello/tests
```

The generated tests call the actual server, increment durable state, restart it,
and verify the value survives. A second test starts with fresh isolated state.
`init` requires an empty/new directory and never overwrites an existing project.
`dev` validates the initial source/config before serving, then delegates live
reload, logs and process signals to `celld dev`. `--port` and `--logs` are supported.

`install` resolves the latest GitHub release once, downloads the current platform's
archive and manifest from that exact tag, verifies both compressed and executable
hashes, checks the reported version, and generates types from that executable.
The completed installation becomes active atomically. A failed download leaves
the previous selection intact. These are HTTPS/checksum checks against GitHub
release assets, not independent code signing; macOS binaries are not notarized.

For reproducible environments, pin both the tooling Git revision and binary:

```sh
# Replace REVISION with a reviewed commit or release tag.
python -m pip install 'pycelld[test] @ git+https://github.com/sambhav/pycelld.git@REVISION'
pycelld install --version 0.4.1-pycelld.4
```

There is no published PyPI package required. Git downloads the Python tooling;
it does not compile Rust. Installation storage defaults to `~/.cache/pycelld`;
`PYCELLD_CACHE_DIR` overrides it. `PYCELLD_BINARY` or `pycelld --binary PATH ...`
selects an existing binary/custom host. Otherwise the active installation is
used, followed by a `celld` found on `PATH`. `pycelld types DIRECTORY` refreshes
editor declarations after changing binaries, including custom host modules.

## Compatibility checks

`pycelld check PROJECT` reads the project's JSON/JSONC config and the Python entry
and imported modules. Common unsupported imports, generators, async iteration,
syntax errors and unavailable native bindings produce `file:line:column`
diagnostics with suggested changes. Project Python never executes in CPython.
The selected binary's generated types identify registered custom host modules.

The final check is `celld deploy PROJECT --dry-run --json`: the same bundler,
native compiler and configuration validator used by deployment, with no server,
bucket writes or `esbuild` for Python. This is not exhaustive static analysis:
unsupported runtime attribute access, input-dependent behavior and handler errors
still need tests. Some native compiler errors report transformed module locations;
the preflight supplies original locations for its listed diagnostics. Host
resource settings and network policies are still validated by the selected host.

Workers use a Python subset. Installing a CPython package into the development
venv does not make it importable inside Monty. Use supported standard-library
modules, bundled project Python, or embedding-host extensions. For HTTP use
`ctx.fetch`, with an explicitly configured host policy. TypeScript resource
bindings are rejected by this tooling until Python exposes their APIs.

## Tests and upstream APIs

Installing the package registers its pytest plugin, including `--celld-binary`.
Source-only callers can also enable
`pytest_plugins = ["pycelld.pytest_plugin"]` in `tests/conftest.py`:

```python
from pathlib import Path

PROJECT = Path(__file__).resolve().parents[1]

def test_counter(celld_worker):
    worker = celld_worker(PROJECT)
    assert worker.call("increment") == 1
    worker.restart()
    assert worker.call("increment") == 2
```

`celld_worker(project, binary=..., env={...})` copies a project into its own
temporary directory, excluding development state, then checks and starts it.
`worker.call(name, arguments)` sends JSON POST requests; `worker.request(path,
method=..., body=..., headers=...)` exposes status, headers, body, `text` and
`json()`. `worker.logs` holds server diagnostics. The fixture terminates the
supervisor and child processes and removes its copy on success, failure, and
startup exceptions. Restart preserves only the fixture's copy. Process-global
`CELLD_*` settings are removed; pass host policy/settings explicitly via `env`.

The `fetch_server` fixture provides a real loopback upstream:

```python
def test_denial(celld_worker, fetch_server):
    fetch_server.respond("/catalog", {"items": [{"name": "tea"}]})
    worker = celld_worker(PROJECT)
    # Supply the URL to your application/config, then invoke its fetch handler.
    # A denied fetch must leave fetch_server.requests empty.
```

It records request methods, paths, headers and bodies. Configuring fixture
responses does not grant network access. For permitted traffic, configure your
embedding host's middleware explicitly, or pass an operator-managed policy path
when using a binary with runtime Python network policies. The
[API cache example](../examples/python-api) shows validation, persistence and
default-denial testing through this fixture.

Alarms and timers use celld's real clock and durable scheduler. Schedule an alarm
through an application handler and use
`worker.wait_until(lambda: worker.call("fired"), timeout=15)` to wait with a bounded
deadline. There is no fake-time or forced-alarm API: injecting one would bypass
the behavior the tests should verify. Set short alarm intervals in test inputs.

Non-pytest callers can use `with pycelld.testing.Worker(project) as worker:` and
`with pycelld.testing.FetchServer() as upstream:`. The repository's broad native
HTTP suite uses the same worker lifecycle.
