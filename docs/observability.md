# Python output and traces

`print()` works in the supplied binary, including before and after `await`,
in constructors, and immediately before an exception. Use `celld dev PROJECT
--logs` to see JSON log records. `print` is INFO; normal Rust log filtering also
applies. No collector or `CELLD_OTEL` setting is needed for local logs.

```python
async def run(ctx):
    print("started")
    with ctx.span("calculate", fields={"version": 1}):
        await ctx.sleep(0.01)
        ctx.log("finished", level="info", fields={"rows": 12})
```

`ctx.log` accepts `debug`, `info`, `warn`, and `error`. User fields remain inside
`fields`; the host attaches `execution` with worker, deployment, runtime instance,
invocation, root/parent invocation and durable object identities. Editing Python
context properties or supplying an `execution` user field cannot replace these
values. Display names longer than 128 bytes are abbreviated in logs; invocation
IDs remain the correlation key. Principal claims, secrets, headers and environment variables are never
copied automatically. Code can still explicitly print its own sensitive data.

`ctx.span` is a synchronous context manager that can surround awaits. Each custom
span is a child of the invocation span, including nested custom spans. It measures
wall time; the context manager marks exceptions as failures. It does not change
the parent of outbound fetches, durable calls, or other custom spans. Cancellation
may discard an unfinished span; there is no durable delivery guarantee.

Exceptions produce an ERROR log with original package filenames, line numbers,
source lines and exception details. Generated linker frames are omitted. HTTP
500 responses hide the message by default. Set `CELLD_PYTHON_PUBLIC_ERRORS=1`
in the supplied binary, or use `Monty::with_public_errors(true)` in an embedding,
to expose messages during development. Internal durable-call errors retain the
exception message for normal Python catch semantics. Source tracebacks remain in
logs regardless of this response setting.

## Bounds and delivery

Print fragments are combined into lines, split before 8 KiB, and flushed on
interpreter yields, completion and errors. A partial line can therefore become
separate records across awaits. Each invocation permits at most 64 KiB or 256
diagnostic events, whichever is reached first. Excess output is discarded with
one truncation warning; it does not fail the request. One bounded exception
record is reserved outside that budget. Total retained output is at most 64 KiB
plus one warning and an 8 KiB error record, excluding fixed host identity fields.
Log envelopes are capped again to 8 KiB while preserving valid JSON and trusted
identity. Custom spans allow names up to 128 bytes, fields up to 4 KiB, 16 open
spans and 128 span starts per invocation. They share the diagnostic event budget.

The existing telemetry queue uses nonblocking `try_send`, bounded batches and
bounded retries. A full queue drops telemetry. Collector failures and retry
backoff run in the exporter task, never in the request path. Exported Python logs
and spans follow the invocation's sampling decision; local logs remain visible.
Custom `Observer` implementations in embedding hosts must preserve this contract.

## OTLP example

Run an OpenTelemetry collector with an OTLP/HTTP receiver on port 4318, then:

```sh
export CELLD_OTEL=1
export CELLD_OTEL_SINK=otlp
export OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
export CELLD_OTEL_FLUSH_MS=1000
export OTEL_SERVICE_NAME=pycelld-demo
celld dev examples/observability --logs
```

The example's outbound fetch requires a host network policy. For a completely
local demonstration, build its small host which permits only `127.0.0.1`:

```sh
cargo build --locked --example observability
python3 tools/monty-checks/observability.py target/debug/examples/observability
```

This starts a local backend and a minimal OTLP receiver, invokes HTTP → durable
object → outbound fetch, and checks trace IDs, parent links, custom spans and
correlated Python logs. It interleaves two object calls and checks their IDs,
then makes the collector fail and verifies requests still complete. The receiver
is an integration-test fixture, not a production collector.

`OTEL_EXPORTER_OTLP_HEADERS` and `OTEL_EXPORTER_OTLP_TIMEOUT` configure collector
authentication and timeout. `OTEL_TRACES_SAMPLER=parentbased_traceidratio` with
`OTEL_TRACES_SAMPLER_ARG=0.1` records 10% of root traces while preserving upstream
sampling decisions. For the bucket sink, set `CELLD_OTEL_SINK=bucket`; Python
logs retain structured JSON bodies and custom spans have an optional `attributes`
JSON column. OTLP additionally promotes execution IDs to `celld.*` log attributes
and exports custom-span metadata as `python.attributes`.

Deployments require `monty-observability-v1`; older artifacts continue to load.
