# Execution identity and limits

Every Python invocation receives `ctx.execution`:

```python
def identify(ctx):
    return {
        "worker": ctx.execution.worker_id,
        "deployment": ctx.execution.deployment_id,
        "invocation": ctx.execution.invocation_id,
        "instance": ctx.execution.runtime_instance_id,
        "root": ctx.execution.root_invocation_id,
        "parent": ctx.execution.parent_invocation_id,
        "object": ctx.id,
    }
```

`worker_id` is the configured script name. `deployment_id` is the deployed version;
for directly embedded configurations without a version it is `source:` plus the
SHA-256 of the compiled source. A runtime instance gets a random UUID when its
slot is loaded. Each invocation gets a new random UUID, including reused slots,
concurrent calls and alarms. Restarting creates new instance and invocation IDs;
these are not durable storage keys. `ctx.id` remains the separate durable object
name. Neither object IDs nor execution IDs are authorization credentials.

Nested native object calls receive fresh invocation and target instance IDs while
retaining root/parent correlation. The host transports this context separately from
application arguments, including through authenticated peer tunnels. Public headers
and Python mutations cannot supply this context. Alarms start new roots; interpreter
stacks and execution IDs are not persisted with storage.

Rust fetch middleware receives the original `FetchContext.execution` and `limits`,
independent of mutable Python copies. A trusted embedding host can attach a
`VerifiedPrincipal` to `WorkerConfig::with_verified_principal`, or pass it through
the public `Invocation` contract. Descendant calls inherit that principal unless
the target host explicitly supplies one. The supplied binary leaves it absent:
this is an integration point after authentication, not an authentication provider.
Only already-authorized trusted hosts may populate it. The existing peer tunnel
trust boundary applies (authenticated fleet peers and their private transport).

## Host ceilings

Set process environment `CELLD_NATIVE_LIMITS` before starting celld:

```sh
export CELLD_NATIVE_LIMITS='{"cpu_ms":100,"wall_ms":30000,"max_operations":10000,"max_payload_bytes":1048576}'
```

Omitted fields use the defaults shown. Unknown fields, zero, negative numbers and
out-of-range values fail validation. Worker `vars`, request arguments and Python
assignments cannot raise the ceilings. `ctx.limits` exposes their effective native
values for inspection. The existing global `CELLD_HANDLER_BUDGET_S` can further
lower the wall budget.

| Limit | Allowed values | Enforcement |
| --- | --- | --- |
| `cpu_ms` | 1–60,000, no greater than wall time | Cumulative active Monty execution across suspension/resumption; time-limit errors cannot be caught by Python |
| `wall_ms` | 1–300,000 | Existing host driver deadline includes waiting for input and asynchronous operations; timeout cancels operations and releases interpreter state/gates |
| `max_operations` | 1–1,000,000 | All Monty host/extension/filesystem calls; the host independently counts emitted native operations |
| `max_payload_bytes` | 1–1,048,576 | Requests, responses, host calls and replies; includes HTTP URL/method/headers and bodies, JSON values and filesystem buffers |

The CPU default remains 100 ms, recursion depth remains 100, and the operation
and maximum body defaults remain 10,000 and 1 MiB. Native stateless wall time now
defaults to 30 seconds (previously the global handler default was 300 seconds);
durable invocations already had a 30-second cap. Payload accounting now includes
metadata such as headers, so a body exactly 1 MiB will exceed the default once
other request fields are included. Filesystem storage quotas are unchanged.

These are cooperative interpreter and I/O limits, not OS process isolation or a
memory quota. Trusted synchronous Rust extensions and middleware must remain
bounded and nonblocking; they cannot be preempted by Monty's interpreter timer.
CPU accounting excludes time suspended in the host. Host wall deadlines cancel
pending I/O, and an execution dropped by an embedding caller is cancelled.
The host advertises `monty-execution-v1`; see [builds](build.md) for the current
deployment requirement. Older artifacts still load with the host's configured limits.
