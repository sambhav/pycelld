# Native Monty and TypeScript HTTP comparison

Monty runs entirely through the native worker backend: Python artifacts, HTTP
routing and bodies, interpreter continuations, timers, HTTP client, storage and
durable method dispatch. Python workers allocate no V8 isolate. TypeScript uses
celld's usual V8 worker backend in the same binary.

## Run

```sh
cargo xtask build
python3 tools/monty-checks/bench.py target/lab/celld \
  --seconds 1 --repeats 3 --concurrency 1 16 \
  --output tools/monty-checks/bench/results-native.json
```

Python 3.12+ on Linux runs the standard-library benchmark driver. Put esbuild
on PATH for the TypeScript build (measured with 0.25.9). Monty itself requires
neither Node/esbuild nor an installed Python interpreter to build or serve.

The binary uses Rust 1.98.1 and the default optimized `lab` profile (thin LTO).
Both runtimes use one stateless pool slot. The test rotates runtime order and
runs three 1-second repetitions at concurrency 1 and 16, after a 0.25-second
warmup. Up to four Python client processes use keep-alive connections and check
every response. Counter replies must be unique and increasing. No build or
other test suite ran during these measurements.

The counter updates the same ID through an explicit SQLite transaction; the
TypeScript method uses synchronous storage.kv inside transactionSync. Both
methods hold the native input gate. Their storage encodings differ (JSON text
and V8 structured clone), so this compares the real interfaces as a whole.
The empty durable method measures dispatch without application storage work,
but still includes interpreter execution, serialization, routing and gates.
The fetch case POSTs a 16 KiB JSON value to the same server's echo handler and
returns its response; it measures two HTTP handlers plus an actual HTTP client
round trip. Binary buffers stay native throughout the Monty HTTP path.

## Results

**72 samples, 306,946 validated responses, zero errors.**

Measured on 2026-09-07 using the external runtime crates and patched upstream build.

Median requests/second across the three repetitions:

| Concurrency | Case | Monty | TypeScript | Monty / TS |
| --- | --- | ---: | ---: | ---: |
| 1 | Text greeting | 6,660 | 5,722 | 1.16× |
| 1 | 16 KiB JSON echo | 4,805 | 2,305 | 2.08× |
| 1 | Empty durable method | 1,660 | 1,993 | 0.83× |
| 1 | Durable counter | 462 | 457 | 1.01× |
| 1 | 10 ms timer | 86 | 86 | 1.00× |
| 1 | 16 KiB HTTP fetch | 1,761 | 1,408 | 1.25× |
| 16 | Text greeting | 14,115 | 17,504 | 0.81× |
| 16 | 16 KiB JSON echo | 9,352 | 5,885 | 1.59× |
| 16 | Empty durable method | 5,705 | 8,737 | 0.65× |
| 16 | Durable counter | 1,992 | 2,608 | 0.76× |
| 16 | 10 ms timer | 1,347 | 1,356 | 0.99× |
| 16 | 16 KiB HTTP fetch | 3,017 | 2,520 | 1.20× |

At concurrency 16, median per-sample p95 latency (milliseconds):

| Case | Monty | TypeScript |
| --- | ---: | ---: |
| Text greeting | 1.62 | 1.43 |
| 16 KiB JSON echo | 2.25 | 3.82 |
| Empty durable method | 6.00 | 3.11 |
| Durable counter | 13.15 | 9.58 |
| 10 ms timer | 12.71 | 12.76 |
| 16 KiB HTTP fetch | 6.25 | 7.48 |

These are short, warm local measurements using a dev bucket. They do not
measure remote fleet durability, distributed ownership, cold startup, memory,
or streaming. The node binary still includes and initializes V8 for TypeScript
support; Python worker materialization and invocation do not enter it.
No startup or memory improvement is claimed from this throughput test.

The rankings depend on load and payload. Removing JavaScript from Monty's
execution path does not remove interpreter, serialization, routing or storage
costs. Do not attribute a difference against TypeScript to a single component.

The runner reported 9 CPUs in affinity and CPU quota `800000 100000`; affinity was not pinned. Client CPU peaked at 1.15 aggregate cores. Server CPU and RSS were not measured because process statistics are not reliable in this environment.

[Raw samples](results-native.json) include the runner timestamp and binary SHA-256:
`032f996829b5b5bbfc36055685a57fb663351ffd9c555dbf13714a8d0c53415c`.

Before this benchmark, the HTTP test hit the container memory admission limit
because compiler artifacts occupied about 19 GB of file cache. Releasing that
cache allowed the same binary to pass. A prior pre-extraction benchmark also
had an unexplained HTTP 500; its cause remains unknown. The current driver
captures error bodies and server logs on failure.
