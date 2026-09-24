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
- `MOYODB_BENCH_IDB_DURABILITY=strict|relaxed|default` (default `strict`, matching MoyoDB's flush-per-commit)
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
- explicit durability: IndexedDB readwrite transactions pass `{ durability }` (default `strict`), and MoyoDB always flushes WAL, main file, and manifest before a commit resolves.

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

## Future baseline: SQLite WASM + OPFS

SQLite WASM + OPFS is a serious future comparison target because the SQLite project publishes WebAssembly/JavaScript documentation and documents persistent browser storage options via OPFS. This suite does not claim SQLite comparison results until a reproducible workload and raw JSON output are added.
