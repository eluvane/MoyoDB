# Browser benchmarks

This directory contains the browser benchmark suite for MoyoDB. It measures browser-runtime paths: TypeScript SDK, Worker transport, WebAssembly engine bindings, and OPFS persistence when a workload uses persistent storage.

Native Rust Criterion benches are useful for engine-core microbenchmarks, but they do **not** prove browser performance. Browser results depend on the browser implementation, storage backend, quota state, cache state, release/debug build mode, value size, batch size, transaction boundary, and whether the benchmark is using SDK calls, Worker-local engine calls, or raw OPFS.

## Commands

```bash
cd packages/sdk
npm run build:wasm:release
npm run bench:browser     # smoke profile, MoyoDB + IndexedDB, Chromium project
npm run bench:indexeddb   # smoke profile, IndexedDB only
npm run bench:opfs        # smoke profile, MoyoDB/OPFS only
npm run bench:report      # summarize raw JSON files from bench/results
```

`npm run build`, `npm run dev`, `npm run dev:test`, and `npm run test:e2e` all build the WASM package with the release script. `npm run build:wasm:dev` exists only for local debugging and must not be used for published benchmark numbers.

The Playwright benchmark test is skipped during normal `npm run test:e2e`. It runs when invoked by one of the benchmark npm scripts or when `MOYODB_RUN_BENCH=1` is set.

Optional environment variables:

- `MOYODB_BENCH_PROFILE=smoke|standard|full` (`full` is the practical suite, not the million-row rows)
- `MOYODB_BENCH_ENGINE=all|moyodb|indexeddb`
- `MOYODB_BENCH_WORKLOADS=bulk_insert_1m_batched_10000,random_get_10k_from_1m_bulk`
- `MOYODB_BENCH_SAMPLE_COUNT=1`
- `MOYODB_BENCH_WARMUP_COUNT=0`
- `MOYODB_BENCH_WORKLOAD_TIMEOUT_MS=300000`
- `MOYODB_BENCH_IDB_DURABILITY=strict|relaxed|default` (default `strict`, matching MoyoDB's durable WAL flush per commit)
- `MOYODB_BENCH_GIT_SHA=<sha>` overrides the revision the launcher reads from `git` (or `GITHUB_SHA` in CI)
- `MOYODB_CHROMIUM_EXECUTABLE_PATH=/path/to/chromium` for local systems without Playwright-managed browsers
- `MOYODB_DISABLE_VIDEO=1` for environments that do not have Playwright's bundled ffmpeg

## Manual browser run

```bash
cd packages/sdk
npm run dev
# open http://127.0.0.1:4173/bench/browser-bench.html
```

Use the page controls to run `smoke`, `standard`, or `full` profiles and export raw JSON.

## Timing rules

Every workload has an optional `prepare()` step, a measured `run()` step, and an untimed `verify()` step that runs before cleanup.

The measured region uses `performance.now()` inside the browser page and excludes:

- deterministic key/value generation;
- random key generation;
- database delete/cleanup;
- database open/create unless the workload name explicitly says `open`;
- preload for read/scan workloads;
- report generation and JSON stringify;
- Playwright `page.evaluate()` per operation.

For million-row read/scan workloads, preload uses one setup transaction so the read benchmark is not blocked by the separate batched-write stress path. Compare those rows as read latency only; use the insert rows for write-path numbers.

Warmup samples are recorded separately and excluded from percentiles. Raw measured samples are preserved in `bench/results/*.json`.

## Diagnostic layer workloads

The smoke profile includes small diagnostics so regressions can be assigned to the right layer before optimizing code:

| Workload                         | Layer isolated                                                                        |
| -------------------------------- | ------------------------------------------------------------------------------------- |
| `noop_js_loop_1m`                | JavaScript loop overhead only.                                                        |
| `noop_worker_roundtrip_10k`      | Legacy raw Worker postMessage/request-response latency without SDK/WASM/OPFS.         |
| `worker_roundtrip_noop`          | Internal Worker protocol-style no-op roundtrip.                                       |
| `worker_roundtrip_small_payload` | Internal Worker protocol-style 32-byte payload roundtrip.                             |
| `worker_roundtrip_256b_payload`  | Internal Worker protocol-style 256-byte binary payload roundtrip.                     |
| `worker_roundtrip_64kb_payload`  | Internal Worker protocol-style 64 KiB structured-clone payload roundtrip.             |
| `worker_binary_transfer_64kb`    | Internal Worker protocol-style 64 KiB transferred payload roundtrip.                  |
| `noop_wasm_call_100k`            | Repeated JS-to-WASM method dispatch inside a Worker.                                  |
| `encode_decode_10k_256b`         | Benchmark key/value allocation and byte-copy cost.                                    |
| `opfs_raw_write_100mb`           | Raw OPFS SyncAccessHandle sequential write throughput.                                |
| `opfs_raw_read_random_10k`       | Raw OPFS SyncAccessHandle random read throughput.                                     |
| `sdk_put_1k_single_calls`        | Smoke-sized public SDK single-call write loop; intentionally shows per-call overhead. |
| `sdk_bulk_put_10k`               | Public SDK bulk write path with data generation/setup excluded.                       |
| `engine_stage_put_10k_rollback`  | Worker-local WASM transaction staging without commit/fsync.                           |
| `engine_bulk_put_10k`            | Worker-local WASM + engine commit path, bypassing public SDK payload transfer.        |
| `indexeddb_bulk_put_10k`         | IndexedDB single-transaction baseline with setup excluded.                            |

The diagnostic names are intentionally explicit. Do not publish one of them as an end-to-end product benchmark without explaining which layer it isolates.

## End-to-end workloads

Representative write/read/scan rows include:

- `open_empty_db`
- `bulk_insert_10k` (automatic). `bulk_insert_100k`, `bulk_insert_1m`, `bulk_insert_1m_batched_1000`, `bulk_insert_1m_batched_10000`, `bulk_insert_1m_single_tx`, and `cold_insert_1m_single_tx` are opt-in (`manual`)
- `sdk_put_10k_single_calls` opt-in. Ten thousand separate commits ran for about an hour on one thread
- `point_get_random_10k` (sequential), `point_get_random_10k_pipelined`, and `point_get_random_10k_bulk` (MoyoDB only) are automatic. `point_get_random_100k` and `point_get_random_1m` are opt-in
- `point_get_random_1m_preloaded`, `random_get_10k_from_1m`, `random_get_10k_from_1m_pipelined`, `random_get_10k_from_1m_bulk`, `range_scan_1000_from_1m`, `batch_tx_100k_values_256b`, and `cold_open_after_100k` are opt-in
- `range_scan_100`, `range_scan_1000`, `range_scan_10000` run against a 10k-row database
- `reverse_scan_limit_1` reads the last row with a reverse scan bounded to one result
- `small_tx_1000_commits` is one sample
- `large_value_64kb`, `large_value_1mb`
- `recovery_after_dirty_close`
- `snapshot_export_import`
- `worker_roundtrip_overhead`

The `bulk_insert_1m_single_tx`/`cold_insert_1m_single_tx` row is a pathological large single-transaction probe for the current architecture. It must be reported next to batched rows, commit diagnostics, and IndexedDB transaction-boundary notes. It is not the headline browser benchmark by itself.

## Fair IndexedDB baseline

IndexedDB is the browser's standard transactional object store. MoyoDB explores a lower-level OPFS-backed storage-engine design for workloads where predictable batch performance and recovery behavior matter.

For comparable workloads, both engines use:

- the same binary keys (IndexedDB orders binary keys bytewise, like MoyoDB) and the same deterministic values;
- the same random index sequence per sample, generated once in `workloads.ts`;
- the same record count, value size, batch size, warmup count, measured sample count, and transaction boundaries;
- explicit durability: IndexedDB readwrite transactions pass `{ durability }` (default `strict`); MoyoDB flushes WAL before a commit resolves and uses bounded checkpoints to install pages and flush the main file and manifest.

Random reads are split by request mode, because issuing requests one at a time and queueing them all measure different things:

| Mode       | MoyoDB                                 | IndexedDB                                           |
| ---------- | -------------------------------------- | --------------------------------------------------- |
| sequential | `await tx.get()` per key               | next `store.get()` issued from the previous success |
| pipelined  | all `tx.get()` calls, then `await` all | all `store.get()` requests queued at once           |
| bulk       | one `tx.getMany()`                     | not applicable: IndexedDB has no multi-key get      |

Range scans use each engine's bulk range API (`tx.scan` versus `getAllKeys` + `getAll`). The reverse scan uses `tx.scan({ reverse: true, limit: 1 })` versus a `prev` cursor stopped after one row.

After every timed sample, `verify()` checks the data against the deterministic dataset: read and scan results in full, and write samples by reading back up to 1,024 evenly spaced records. A mismatch fails the sample. Each result stores one content checksum per measured sample, and the report's content-parity table flags workloads where the engines disagree.

Data generation, opening the database, and preload stay outside the measured regions. The baseline does not intentionally slow IndexedDB down.

## WebKit/Safari handling

The MoyoDB OPFS path requires `FileSystemSyncAccessHandle` in a dedicated Worker. The suite probes that capability in a Worker and skips MoyoDB OPFS workloads only when the current browser/runtime does not expose it. A Playwright WebKit skip is therefore an environment/runtime result, not a claim that Safari as a browser never supports OPFS.

## Output fields

Each report records:

- browser name/version, user agent, platform, timestamp, and webdriver/headless hints;
- the git revision (with a `-dirty` suffix for uncommitted tracked changes);
- SDK build mode, the WASM build profile reported at runtime by the engine's `buildProfile()`, backend path, persistent-context flag;
- IndexedDB and MoyoDB durability settings;
- per-sample content checksums;
- OPFS, Worker, BroadcastChannel, Web Locks, and SyncAccessHandle support flags;
- workload name, record count, key size, value size, batch size, transaction boundary;
- warmup samples, measured samples, p50/p95/p99/min/max/mean;
- notes, skip reasons, and errors.

## Performance-gap pass (2026-10-03)

The base is `MoyoDB-dev` main commit `23feb4d67e9b698ab0a39deca1af8af7608097dc`. The compact [measurement evidence](performance-gap-2026-10-03.json) retains raw samples, warmups, source/WASM fingerprints, transaction shapes, content-parity results, and per-launch variation. The changed variant is that commit **plus this patch**, not another committed revision.

### Comparable browser results

These are complete public-SDK workloads in Linux Chromium 153.0.8010.0, using Vite-served SDK source and release WASM. Four fresh persistent browser profiles ran in before/after/after/before order. Each row had two warmups and five measured samples per launch: ten Moyo samples per variant and twenty IndexedDB controls in total. Both engines retained the stock inputs, timed regions, transaction boundaries, and strict durability. All selected samples passed content verification.

The IndexedDB column is one shared median of all twenty controls. Its values fluctuated between launches; pooling provides a common reference and does not remove host drift. The evidence preserves that spread. These are four browser launches, not twenty independent replications. Fresh empty DB open is **not** cold browser/module startup: the stock harness checks the WASM build profile before sampling.

| Workload                          | Moyo before, ms | Moyo after, ms | Shared IndexedDB, ms | Gap before | Gap after |
| --------------------------------- | --------------: | -------------: | -------------------: | ---------: | --------: |
| 1,000 values, 64 KiB; ten commits |          397.00 |         295.00 |                54.60 |      7.27x |     5.40x |
| 64 values, 1 MiB; eight commits   |          350.35 |         286.65 |                72.35 |      4.84x |     3.96x |
| 1,000 separate commits            |          173.70 |         158.90 |               202.70 |      0.86x |     0.78x |
| 10,000 pipelined reads            |          143.00 |         130.50 |               171.00 |      0.84x |     0.76x |
| Reverse scan, limit one           |            0.70 |           0.60 |                 0.45 |      1.56x |     1.33x |
| Fresh empty DB open               |           50.25 |          34.15 |                 0.50 |    100.50x |    68.30x |

Large-write medians improved in both A/B pairs. Their measured reductions are 25.7% and 18.2%. The other rows require more caution: pipelined-read direction reverses in the second pair, and the observed startup change exceeds the separately measured 2–3 ms capability probe. Do not attribute every movement in this table to the patch. The original external timings are not the control for this experiment; the current revision and this browser/storage environment already put separate commits and pipelined reads ahead of IndexedDB.

### Cause and preserved physical work

Separate, instrumented browser runs placed most large-write time inside WASM commit. Temporary Rust stage timers in an isolated diagnostic build measured the following averages over two samples after one warmup. These diagnostic timings are **not** the headline A/B samples, and WAL encoding includes both copying and checksum calculation.

| Diagnostic phase                        | 64 KiB before / after, ms | 1 MiB before / after, ms |
| --------------------------------------- | ------------------------: | -----------------------: |
| Commit planning                         |             120.5 / 124.8 |            110.9 / 112.0 |
| WAL encoding                            |              104.9 / 28.1 |              96.7 / 20.1 |
| WAL backend write                       |               15.8 / 20.8 |              14.5 / 15.4 |
| Checkpoint, including its storage calls |               60.7 / 58.4 |              47.3 / 48.1 |
| Complete WASM commit                    |             306.3 / 236.4 |            272.8 / 199.0 |

The page encoder already computes a full CRC with the page checksum field zeroed. The generated-page WAL path now combines that CRC with the 40-byte WAL prefix and the fixed correction for the embedded little-endian checksum. The resulting WAL bytes are identical to the generic encoder. The generic public append APIs still hash arbitrary input in full; read/recovery validation is unchanged. Two compile-time tables occupy 8 KiB and require no runtime initialization.

| Structural measurement per complete workload |     64 KiB before / after |      1 MiB before / after |
| -------------------------------------------- | ------------------------: | ------------------------: |
| Generated pages                              |           34,066 / 34,066 |           33,176 / 33,176 |
| Bytes hashed for WAL page records            |   140,896,976 / 1,362,640 |   137,215,936 / 1,327,040 |
| WAL writes                                   |                   10 / 10 |                     8 / 8 |
| Bytes written to WAL                         | 140,897,456 / 140,897,456 | 137,216,320 / 137,216,320 |
| Main-file writes                             |                 547 / 547 |                 533 / 533 |
| Bytes written to main file                   | 139,534,336 / 139,534,336 | 135,897,088 / 135,897,088 |
| WAL / main / manifest flush calls            |     15 / 5 / 5, unchanged |     16 / 8 / 8, unchanged |

Each generated page therefore loses one redundant 4,096-byte checksum scan; its complete page checksum remains. Exact work-accounting tests exercise the real commit callsite at 1, 8, and 100 values, and differential tests compare complete WAL bytes, validation order, partial writes, and corrupt-input errors. The startup change also removes six synthetic capability-probe storage calls from each open/delete while preserving actual database handle acquisition; it eliminates contention on the shared probe file.

The remaining large-write costs include constructing user and change-feed trees, their required page checksums/copies, and checkpoint work. The enabled change feed retains complete values in its own tree: this workload still writes roughly twice its payload into page images and then persists WAL plus main-file data. Sharing those large payloads would require a separate lifetime/reclamation design. Startup is dominated by Worker/module/WASM initialization in the diagnostic traces. Pipelined reads already use batched transport; the reverse-scan workload retains explicit begin/scan/rollback boundaries.

### Reproduction

Install the locked dependencies and the repository's Rust/WASM/browser tooling. Build release WASM in both the unchanged detached worktree and the patched checkout. From the patched repository root:

```bash
git worktree add --detach ../MoyoDB-before 23feb4d67e9b698ab0a39deca1af8af7608097dc
(cd ../MoyoDB-before && npm ci && npm run build:wasm:release --workspace @moyodb/sdk)
npm run build --workspace @moyodb/sdk

node packages/sdk/scripts/bench-performance-gap.mjs --source-root ../MoyoDB-before --label before-a --samples 5 --warmups 2
node packages/sdk/scripts/bench-performance-gap.mjs --label after-a --sha 23feb4d67e9b698ab0a39deca1af8af7608097dc+patch --samples 5 --warmups 2 --reopen-check
node packages/sdk/scripts/bench-performance-gap.mjs --label after-b --sha 23feb4d67e9b698ab0a39deca1af8af7608097dc+patch --samples 5 --warmups 2
node packages/sdk/scripts/bench-performance-gap.mjs --source-root ../MoyoDB-before --label before-b --samples 5 --warmups 2
```

The runner starts Vite and Chromium in the same process environment and saves raw JSON under ignored `bench/results/`. `MOYODB_CHROMIUM_EXECUTABLE_PATH` can select an installed Chromium. Do not run compilation or other benchmarks concurrently. The optional reopen probe is a separate 128 x 4 KiB clean-close/reopen check for both engines; it is not a power-loss or large-value crash test.

For symmetric scaling, use `--large-value-sizes 4096,16384,65536,262144,1048576 --samples 3 --warmups 1` on each source tree. This retains 64 MiB total payload and eight 8 MiB transactions at every size, using the stock runners and verification. Write verification reads up to 1,024 evenly spaced records, so it samples the smaller-value scaling rows; the two original large-write rows are verified in full.

The executed scaling series improved all five sizes. These medians come from a separate pair of launches with three measured samples per engine/size and one warmup; the IndexedDB reference pools six controls. Raw values and parity results are in the same evidence JSON.

| Value size | Values | Moyo before, ms | Moyo after, ms | Shared IndexedDB, ms | Moyo reduction |
| ---------- | -----: | --------------: | -------------: | -------------------: | -------------: |
| 4 KiB      | 16,384 |           883.5 |          595.8 |               975.85 |          32.6% |
| 16 KiB     |  4,096 |           502.1 |          408.8 |               522.35 |          18.6% |
| 64 KiB     |  1,024 |           400.9 |          334.9 |                71.60 |          16.5% |
| 256 KiB    |    256 |           401.1 |          304.7 |                42.30 |          24.0% |
| 1 MiB      |     64 |           359.2 |          299.4 |                83.35 |          16.6% |

For separate OPFS, Worker-message, startup, and WASM-call accounting, add `--diagnostic --engine-timings --engines moyodb`. This uses instrumentation and disables HTTP cache through routing. Its `diagnostics.records[].elapsedMs` excludes snapshot collection but still includes hooks; the stock `rawSamples` also include snapshot requests. Neither is an uninstrumented comparison. The shipped runner measures public call/storage boundaries; the temporary private Rust stage timers used above are not included in the runtime patch.

Verification performed for this pass: 200 native Rust tests (one pre-existing ignored test), native/WASM Clippy, 128 SDK work tests, 49 OPFS-shim tests, 83 Chromium correctness tests (benchmark and soak opt-ins skipped), one separate ten-second randomized model/reopen/snapshot test, typecheck, scoped formatting/lint, dependency checks, and the complete release SDK/WASM build. The 27 Criterion test-mode smoke cases were not timed native benchmarks. Firefox and WebKit were not run in this environment.

## Future baseline: SQLite WASM + OPFS

SQLite WASM + OPFS is a serious future comparison target because the SQLite project publishes WebAssembly/JavaScript documentation and documents persistent browser storage options via OPFS. This suite does not claim SQLite comparison results until a reproducible workload and raw JSON output are added.
