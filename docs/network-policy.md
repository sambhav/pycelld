# Python network policies

The supplied `celld` binary can load an operator-owned Python network policy.
Without a policy, Python outbound HTTP remains disabled.

```sh
CELLD_PYTHON_NETWORK_POLICY=/etc/celld/network-policy.py celld dev ./my-worker
```

Use the same environment variable on production serving processes. The file is
local operator configuration, separate from deployed worker packages, bindings,
and environment variables in `celld.jsonc`. Workers cannot install or replace it.
Start with the [example policy](../examples/network-policy/policy.py):

```python
def policy(request: dict, context: dict) -> dict:
    if request["scheme"] == "https" and request["host"] == "api.example.com":
        return {"action": "forward"}
    return {"action": "deny", "reason": "destination is not allowed"}
```

Policies run in a separate Monty interpreter with fresh globals for every fetch.
They do not import worker modules or receive worker extensions, process
environment, filesystem access, networking, or other host functions. A policy
must finish synchronously; suspension fails closed. Pure Monty operations and
supported standard modules are available. Use `dict` annotations as above;
the complete editor contract is in [network-policy.pyi](network-policy.pyi).

## Request and context

| Field | Type | Meaning |
| --- | --- | --- |
| `request.url` | `str` | Parsed, normalized absolute HTTP(S) URL |
| `request.scheme`, `request.host` | `str` | Scheme and host without credentials |
| `request.port` | `int` | Explicit port or scheme default |
| `request.method` | `str` | HTTP method |
| `request.headers` | `list[list[str]]` | Ordered name/value pairs, including duplicates |
| `request.body` | `bytes` | Native binary body |
| `context.request_url` | `str` | Originating invocation URL; caller-controlled data |
| `context.execution` | `dict` | Host-assigned worker, deployment, invocation, runtime instance and parent/root IDs; optional verified principal |
| `context.object` | `dict \| None` | Durable `class` and `id`, independent of runtime instance identity |
| `context.alarm` | `bool` | Whether the host dispatched an alarm |
| `context.limits` | `dict` | Effective execution ceilings |

These are dictionary keys: use `context["execution"]["runtime_instance_id"]`.
The host copies execution metadata directly into the policy; changing
`ctx.execution` or `ctx.env` in worker code cannot change it. Runtime instance
IDs are ephemeral: use durable identity for persistent routing. Durable IDs and
worker IDs are identifiers, not proof that the caller is authorized. A principal
is present only when an embedding host has verified and attached one.

## Decisions

- `{"action": "forward"}` forwards the original request. Optional `url`,
  `method`, `headers`, and `body` replace those fields. Omitted fields survive;
  headers replace the entire ordered list, so explicitly append if desired.
- `{"action": "respond", "status": 200, "headers": [], "body": b"ok"}`
  produces a synthetic response without network I/O. Status is required and must
  be 200–599; headers and body default to empty.
- `{"action": "deny", "reason": "destination is not allowed"}` raises a
  catchable worker `RuntimeError`. The optional reason is visible to the worker,
  so do not include credentials or private policy details.

Bodies may be `bytes` or UTF-8 strings in returned decisions. Header pairs may be
lists or tuples. Unknown actions, unexpected keys, invalid URLs, credentials in
URLs, invalid headers/methods, oversized results, Python errors and execution
timeouts deny the call. Runtime policy failures return a generic message that
does not reveal exception text, policy source, request bodies, or credentials.
Policy `print()` output is discarded; policies do not write to worker logs.

Decisions are bounded by 10 ms of active interpreter elapsed time (or the host's
lower CPU ceiling), recursion depth 50, source size 1 MiB, and input/result sizes
of at most 1 MiB or the host's lower payload ceiling. Time checks are cooperative
inside Monty, not hard preemption of an individual VM operation. This is trusted
operator code with bounded execution and transfer sizes, not a separate process
or a hard per-policy heap sandbox. Existing host process memory controls apply.

## Transport and replacement

Forwarding retains celld's transport egress controls, cancellation, timeouts,
body limits and durability gates. Redirects are returned to Python unchanged;
fetching a redirect location requires another policy decision. A URL allowlist
does not replace network-level restrictions on resolved IP addresses.

The binary reads and compiles the file once, before starting the CLI or server.
Missing files, non-UTF-8 input, oversized source, syntax errors, and a missing or
invalid `policy` signature fail startup. Runtime errors in top-level policy code
or a request-specific branch deny that request. Editing or replacing the file
has no effect on the running process: validate the new configuration and restart
serving processes to replace it. There is no live reload or temporary allow-all
state. In-flight requests keep the existing immutable policy until they finish
or their host process stops.

Rust embedders opt in explicitly:

```rust
let policy = pycelld::PythonNetworkPolicy::load("/etc/celld/network-policy.py")?;
let runtime = pycelld::Monty::new().with_network_policy(policy);
pycelld::run(runtime)?;
```

`with_network_policy` appends to the existing middleware chain. Forwarding goes
through later Rust middleware and then the host transport. Deny and synthetic
responses terminate the chain, matching existing Rust middleware semantics.
The stock binary reads the environment variable; `pycelld::run` leaves a custom
host's explicit configuration unchanged.

Run the policy checks against a built binary:

```sh
cargo test -p celld-monty --locked --test network_policy
python tools/monty-checks/network_policy.py target/lab/celld
```
