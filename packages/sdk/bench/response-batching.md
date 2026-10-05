# Same-transaction response batching

Base: `083ad53f6c77cc88c548758185173fdfe6989b5e` in `eluvane/MoyoDB-dev`.

## Scope and cause

`randomPointGets` in `moyodb-baseline.ts` issues pipelined `get` calls in one
readonly transaction. `RequestScheduler` correctly serializes that transaction,
but the old `ResponseQueue` microtask flush ran before the next serialized
operation's reply was ready. Request batching therefore still produced one
Worker response message per get.

Only response delivery changes. A bounded queue coalesces successive microtask
completions until a task boundary. A fully answered input batch flushes
immediately; a slow or ignored sibling cannot hold ready replies indefinitely.
The fallback uses one lazy, reusable MessageChannel, closed on server disposal.
The existing 128-message / 1 MiB bounds and response snapshot/ownership rules
remain in effect. Individually oversized replies still travel alone. Single
request envelopes keep the direct response path. Engine execution order,
transaction boundaries, WAL, persistence barriers and storage format are not
changed.

## Structural measurement

Real SDK client/server/protocol; deterministic async API fixture; native
structuredClone at the test message boundary. Values are checked byte for byte.
These are transport counts, not storage or browser measurements.

| Gets, one transaction | Request messages, both | Response messages before | After | Returned bytes, both |
| --------------------: | ---------------------: | -----------------------: | ----: | -------------------: |
|                   128 |                      1 |                      128 |     1 |               32,768 |
|                 1,000 |                      8 |                    1,000 |     8 |              256,000 |
|                10,000 |                     79 |                   10,000 |    79 |            2,560,000 |

All gets still execute individually and in order. No getMany substitution,
reduced payload, skipped validation or merged transaction is used.

## Node Worker component A/B

Linux x64, Node 22.16.0, TypeScript 5.8.3. Two runs of 21 samples per variant,
five warmups per run and alternating before/after execution order. The table
pools the 42 measured samples per variant and reports their medians in ms.
Actual SDK client, protocol and server run across a real Node Worker boundary.
The engine is an in-memory fixture, **not MoyoDB/WASM/OPFS or IndexedDB**.
Timings include the Node EventTarget adapter and message-count instrumentation.

Both variants time begin + all gets + rollback. Compilation, Worker startup,
fixture preload, key preparation and full result verification are outside
both timed regions. Fixture get output allocation/copying remains inside.

| Workload                           | Before ms | After ms | Before / after |
| ---------------------------------- | --------: | -------: | -------------: |
| 2 pipelined x 256 B                |     0.259 |    0.295 |          0.88x |
| 128 pipelined x 256 B              |     3.557 |    2.585 |          1.38x |
| 1,000 pipelined x 256 B            |    11.490 |    9.988 |          1.15x |
| 10,000 pipelined x 256 B           |   136.914 |   94.816 |          1.44x |
| 128 pipelined x 64 KiB             |     5.720 |    4.013 |          1.43x |
| 1,000 sequential x 256 B (control) |    33.207 |   31.670 |          1.05x |

The sequential path is unchanged; its timing difference is not an optimization
claim. Tiny batches show no demonstrated benefit. An additional same-source
A/A run (11 samples, five warmups) measured before/after ratios of 1.04x for
10,000 pipelined gets, 1.02x for sequential gets and 0.88x for two pipelined
gets. Sub-millisecond timings are especially noisy. No timing threshold is
used as a test gate.

## Reproduce

With normal repository dependencies installed, run from the repository root:

```sh
node packages/sdk/scripts/test-transport-work.mjs
# Same tests against a clean checkout of the base; seven new work checks fail:
node packages/sdk/scripts/test-transport-work.mjs --source-root ../MoyoDB-before/packages/sdk/src
node packages/sdk/scripts/bench-response-batching.mjs ../MoyoDB-before/packages/sdk/src packages/sdk/src --samples 21 --warmup 5 > transport-ab.json
# A/A control: supply the same source directory twice.
```

The benchmark emits source hashes, raw samples, medians, p95s, actual get and
message counts, result bytes and transaction counts. Run twice to
reproduce the sampling procedure above. The before directory must contain the
base commit, not a hand-reimplemented old algorithm.

## Verification and remaining gap

35 transport tests pass. The original source passes the pre-existing 22 tests;
with the extended suite it fails the seven new same-lane batching assertions.
Coverage includes full content and order, partial-view snapshots, batch byte
limits, a blocked sibling, independent lanes, exclusive barriers, per-request
errors, transfer fallback, timeouts, lazy channel allocation/reuse and disposal.
Strict type-checking was run on the transport modules and their actual API/type
dependencies. Node syntax and patch applicability/whitespace checks were run.

The available environment has no Rust/Cargo or network installation access,
and Chromium navigation fails with ERR_BLOCKED_BY_ADMINISTRATOR. Therefore
Rust, WASM, crash/recovery, full SDK/browser tests and a fresh comparable browser
baseline were NOT run. Configured Prettier/Oxlint were not installed either.
The large-value write, commit, reverse-scan and startup paths are not optimized
by this patch. Their dominant current costs have not been established here.

There is no measured new MoyoDB-versus-IndexedDB gap for any of the six user
workloads. In particular, do not substitute the transport timings for the
242 ms browser read result or claim that the 11x/46x gaps were closed. The
unchanged comparable suite in `README.md` still needs A/B execution on the
same browser/device and with the same durability settings and content checks.
