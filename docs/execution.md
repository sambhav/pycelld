# Applications, identity, and execution limits

The host assigns application identity and selects a fresh limit snapshot for each
native invocation. Updating an operator policy changes subsequent invocations
without restarting the server, reloading compiled workers, or resetting durable
state. Python can inspect its snapshot but cannot change host authority.

## Identity model

A project is the operator's logical tenancy grouping. An application belongs to a
project and can have multiple stages and workers. Stage and tier are independent,
optional strings: for example `production` / `preview` and `free` / `pro`.
Custom labels support your own vocabulary, such as organization, region,
cost center, workspace, or environment.

| Field | Meaning and lifetime |
| --- | --- |
| `ctx.execution.application.project_id` | Operator-assigned project/tenancy ID. Scope application names under this ID. |
| `application.application_id` | Logical application within that project, independent of deployments and processes. Several worker IDs can map to the same application. |
| `application.stage` | Optional release environment, such as production, preview, or test. It is not a billing tier. |
| `application.tier` | Optional service/plan class used to select limits. It does not itself grant permissions. |
| `application.labels` | Up to 16 custom string dimensions. Exact-match policy rules use `labels.NAME`. |
| `ctx.execution.worker_id` | Deployed/configured script name, used as the trusted lookup key for the mapping. |
| `deployment_id` | Deployment version, or `source:` plus a SHA-256 for directly embedded source without a version. |
| `runtime_instance_id` | Random ID of the loaded native worker slot. Reused across calls; changes when that slot is replaced. Not a machine ID or durable storage key. |
| `invocation_id` | New random ID for each HTTP call, durable method call, or alarm. The resource accounting unit. |
| `root_invocation_id` | First invocation in a nested call tree. Correlates work; does not create a shared distributed resource budget. |
| `parent_invocation_id` | Immediate caller's invocation ID, or `None` for a root. |
| `policy_revision` | Operator-supplied revision of the snapshot selected at admission. Use a new value for every update. `legacy` means fixed environment defaults. |
| `principal` | Optional authenticated caller supplied by a trusted embedding host. Independent of the application's ownership/placement. |
| `ctx.id` | Durable object name, scoped by the existing worker/class storage namespace. Independent of ephemeral execution IDs. |

A principal has `subject` (authenticated caller ID), optional `tenant_id` (that
caller's tenancy), and `claims` (verified authentication/authorization attributes).
The stock binary does not authenticate arbitrary request headers into principals.
An embedding host may use `WorkerConfig::with_verified_principal` after
verification. Descendants inherit the principal unless the target host supplies
one. Caller tenancy and target project may differ; authorization must check that
the caller may invoke that project/application.

**These mappings are identity and policy metadata, not new storage or access
control boundaries.** Changing a worker's project, application, or stage does not
move, rename, or isolate its stored objects. Deploy separate worker namespaces
when stages or tenants need separate data. Request headers, worker `vars`, Python
arguments, and mutable `ctx` copies cannot select authoritative identities or
raise budgets. A shared deployed worker does not automatically become a separate
application for each caller; use separate mappings/workers or a trusted embedding
integration that authenticates and assigns each `Invocation`.

Nested native calls receive fresh invocation/target-instance IDs, retaining
root/parent correlation via trusted host routing and authenticated peer tunnels.
Application placement and policy are resolved at the **target**; they are not
inherited from the caller. Alarms start new roots. Python mutations cannot forge
the metadata seen by fetch middleware or the diagnostic sink. Logs and spans
include project/application/stage/tier and policy revision; custom labels and
principal claims are not automatically logged.

## Live operator policy

Start celld with a local operator-owned JSON file:

```sh
CELLD_EXECUTION_POLICY=/etc/celld/execution-policy.json celld dev ./orders
```

See the complete [example policy](../examples/execution-policy/policy.json). Its
`workers` keys must exactly match deployed script names (`name` in wrangler
configuration):

```json
{
  "revision": "limits-42",
  "defaults": { "cpu_ms": 100, "wall_ms": 30000, "max_operations": 10000 },
  "workers": {
    "orders-api-prod": {
      "project_id": "acme",
      "application_id": "orders",
      "stage": "production",
      "tier": "pro",
      "labels": { "region": "eu" }
    }
  },
  "rules": [
    { "matches": { "tier": "pro" }, "limits": { "cpu_ms": 500 } },
    { "matches": { "project_id": "acme", "labels.region": "eu" },
      "limits": { "max_operations": 20000 } }
  ]
}
```

Resolution: defaults → all matching rules, in array order. Later rules override
only the fields they set. Selectors within a rule are ANDed. Built-in selectors
are `project_id`, `application_id`, `stage`, and `tier`; custom dimensions use
`labels.NAME`. No wildcards, executable expressions, or request-derived values.
An empty `workers` map denies all native invocations. Unmapped workers are denied;
there is no permissive fallback. Limits are per invocation, not per project per
second or a monthly tier allowance. A later rule can raise a prior rule's value,
so this is an operator-owned ordered policy, not a hierarchy of immutable tenant
caps.

Write a complete new file next to the original and atomically rename it over the
configured path. A background controller checks contents every 250 ms and swaps
only complete, validated snapshots. Worker threads do no policy file I/O. Keep
this file outside the watched application source directory to avoid a development
source reload. Initial invalid configuration prevents startup. A missing,
malformed, oversized, or unsupported update blocks new native invocations with
HTTP 503 after detection; it does not revert to weaker defaults. A valid update
restores admission automatically. Existing calls retain their original snapshot
and deadline, including across I/O resumes. This is not immediate revocation of
already-running work. File errors are logged once per changed error; successful
updates log the revision.

The file is limited to 1 MiB, 4,096 mappings, and 256 rules. Identity strings use
1–128 UTF-8 bytes with no control characters. Unknown fields, invalid selectors,
and invalid resolved limits reject the complete policy. In a fleet, distribute
the file to every process/node: local reloads are not a globally atomic update.
Use `policy_revision` to observe which snapshot served a request.

Embedders can install an `ExecutionPolicy` before startup with
`pycelld::celld::native::replace_execution_policy(policy)` and call it again from
their trusted control plane while workers run. Invalid programmatic replacements
return an error and leave the previous policy intact. Do not mix programmatic
updates with the file controller. The portable runtime API also exposes
`ExecutionPolicyStore` for hosts constructing `Invocation` directly. This change
does not add an unauthenticated HTTP administration endpoint.

For backwards compatibility, without a live policy file or programmatic policy,
`CELLD_NATIVE_LIMITS` still supplies fixed process-start defaults:

```sh
export CELLD_NATIVE_LIMITS='{"cpu_ms":100,"wall_ms":30000,"max_operations":10000,"max_payload_bytes":1048576,"max_recursion_depth":100}'
```

These legacy defaults map workers to project `default`, application = worker ID,
with no stage/tier/labels. Setting both environment options is an error. Legacy
environment mode is not dynamically editable; select live policy mode to update
limits without restarting.

## What is counted and enforced

| Limit | Default / range | Accounting and enforcement |
| --- | --- | --- |
| `cpu_ms` | 100 / 1–60,000; no greater than `wall_ms` | Monty's cumulative elapsed time while executing interpreter bytecode, across all start/resume segments. Uses monotonic `Instant`, **not OS thread CPU time**: descheduling during a segment counts; waiting suspended on fetch/sleep/storage does not. Cooperative VM/long-loop checks raise a fatal resource error. Trusted Rust callbacks, network policy evaluation, compilation, and host serialization are outside this meter. |
| `wall_ms` | 30,000 / 1–300,000 | Elapsed time from native admission, including request-body collection, interpreter work, and asynchronous I/O waits. Shared host deadlines cancel pending work and release continuations/gates. Earlier routing/admission queues and compilation are outside it. `CELLD_HANDLER_BUDGET_S` and existing host/durable deadlines can further reduce the effective deadline. |
| `max_operations` | 10,000 / 1–1,000,000 | One per external-function or OS suspension handled by the Monty session: fetch attempts (including denied/synthetic fetches), storage, files, durable calls, sleep, now, UUID, structured logs/spans, and registered extension callbacks. Retries are new operations. The host independently checks emitted `HostCall`s; these are a subset, not an additional charge. Python arithmetic/loop iterations/bytecodes are not operations. `print` has a separate output budget. Failure occurs before dispatching operation N+1. |
| `max_payload_bytes` | 1,048,576 / 1–1,048,576 | Ceiling on **each** inbound/outbound request, response, host-call arguments, and reply, not a cumulative transfer budget. HTTP includes UTF-8 URL/method/header bytes plus body bytes; JSON uses encoded length; native extension values use deep host-size accounting. The host bounds streamed body collection and rechecks the complete value. Existing conversion/filesystem ceilings still apply. |
| `max_recursion_depth` | 100 / 1–100 | Interpreter Python call-stack depth, including generated wrappers/helpers. Monty's recursion checks enforce it. Very small limits can prevent ordinary wrapped handlers from starting. |
| `max_memory_bytes` | `None` / configured values rejected | There is currently **no per-invocation heap or RSS quota** in the shared-process backend. Setting a value fails validation; it is never silently ignored or described as enforced. |

Every HTTP invocation, durable method invocation, and alarm gets a fresh budget,
even when the same compiled slot serves them concurrently. CPU and operation
counts survive suspension; there is no reset on `await`. A nested durable call
uses one operation in the caller and receives a separate target invocation
budget. The parent wall budget includes waiting for the child. Root IDs do not
aggregate child CPU/operations, especially across peers. Thus these are
application-tagged **individual invocation** limits, not a cap on the whole
application call tree, aggregate concurrency, rate, or billing usage.

`ctx.limits` exposes the selected snapshot. It is not live usage telemetry and
does not retroactively change when the policy updates. The global host deadline
can be lower than the displayed `wall_ms`. CPU exhaustion is fatal even when
Python tries to catch exceptions. Wall cancellation cannot preempt a synchronous
trusted Rust callback; extension authors must keep callbacks bounded and
nonblocking. Recursion errors follow Python exception semantics.

Print output remains bounded independently at 8 KiB/event, 64 KiB/invocation,
and 256 events (with reserved error reporting). Filesystem quotas remain 1 MiB
per file, 16 MiB per durable object, and 1,024 entries. These storage/payload/output
limits do **not** bound the live Python heap or the host process's memory.

### Memory work still required

Tracked in [memory enforcement #31](https://github.com/sambhav/pycelld/issues/31).

The pinned Monty `ResourceTracker` probes process-global `LIVE_MEMORY` minus
`BASELINE_MEMORY`. Its allocator hard limit can terminate the process. Enabling
that mechanism in celld would mix concurrent applications, suspended executions,
V8, and host allocations; one invocation could kill other tenants. It also
requires a different allocator than celld's existing jemalloc/pressure accounting.

A safe implementation needs either invocation-owned allocator accounting that
survives suspension and cross-thread frees, with fallible VM allocation paths,
or isolated worker processes with bounded host RPC and per-process memory limits.
Test concurrent allocations, cancellation, cross-thread resume/free, large single
allocations, and uncatchable exhaustion before exposing a memory quota. Existing
process pressure/readiness protection remains enabled, but is not tenant memory
isolation. `max_payload_bytes` is not a substitute for this work.

New deployments require `monty-application-policy-v1`; older artifacts still load
under the host's selected policy. See [builds](build.md) for feature negotiation.
