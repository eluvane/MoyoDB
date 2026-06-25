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

- `MOYODB_BENCH_PROFILE=smoke|standard|full`
- `MOYODB_BENCH_ENGINE=all|moyodb|indexeddb`
- `MOYODB_BENCH_WORKLOADS=bulk_insert_1m_batched_10000,random_get_10k_from_1m_bulk`
- `MOYODB_BENCH_SAMPLE_COUNT=1`
- `MOYODB_BENCH_WARMUP_COUNT=0`
- `MOYODB_BENCH_WORKLOAD_TIMEOUT_MS=300000`
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

Every workload has an optional `prepare()` step and a measured `run()` step.

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
- `bulk_insert_10k`, `bulk_insert_100k`, `bulk_insert_1m`
- `bulk_insert_1m_batched_1000`
- `bulk_insert_1m_batched_10000`
- `bulk_insert_1m_single_tx`
- `cold_insert_1m_single_tx` compatibility alias for older result files
- `sdk_put_10k_single_calls` heavy public SDK single-call diagnostic, excluded from default smoke
- `point_get_random_10k`, `point_get_random_100k`, `point_get_random_1m`
- `point_get_random_1m_preloaded` compatibility alias for older result files
- `random_get_10k_from_1m`
- `random_get_10k_from_1m_bulk`
- `range_scan_100`, `range_scan_1000`, `range_scan_10000`, `range_scan_1000_from_1m`
- `small_tx_1000_commits`
- `batch_tx_100k_values_256b`
- `large_value_64kb`, `large_value_1mb`
- `cold_open_after_100k`
- `recovery_after_dirty_close`
- `snapshot_export_import`
- `worker_roundtrip_overhead`

The `bulk_insert_1m_single_tx`/`cold_insert_1m_single_tx` row is a pathological large single-transaction probe for the current architecture. It must be reported next to batched rows, commit diagnostics, and IndexedDB transaction-boundary notes. It is not the headline browser benchmark by itself.

## Fair IndexedDB baseline

IndexedDB is the browser's standard transactional object store. MoyoDB explores a lower-level OPFS-backed storage-engine design for workloads where predictable batch performance and recovery behavior matter.

For comparable workloads, the IndexedDB baseline uses the same record count, fixed-width string keys with the same UTF-8 byte length as MoyoDB keys, value size, batch size, warmup count, measured sample count, and transaction boundaries. It opens the DB and generates/preloads test data outside read/scan measured regions. It does not intentionally slow IndexedDB down.

## WebKit/Safari handling

The MoyoDB OPFS path requires `FileSystemSyncAccessHandle` in a dedicated Worker. The suite probes that capability in a Worker and skips MoyoDB OPFS workloads only when the current browser/runtime does not expose it. A Playwright WebKit skip is therefore an environment/runtime result, not a claim that Safari as a browser never supports OPFS.

## Output fields

Each report records:

- browser name/version, user agent, platform, timestamp, and webdriver/headless hints;
- SDK build mode, WASM build-mode note, backend path, persistent-context flag;
- OPFS, Worker, BroadcastChannel, Web Locks, and SyncAccessHandle support flags;
- workload name, record count, key size, value size, batch size, transaction boundary;
- warmup samples, measured samples, p50/p95/p99/min/max/mean;
- notes, skip reasons, and errors.

## Future baseline: SQLite WASM + OPFS

SQLite WASM + OPFS is a serious future comparison target because the SQLite project publishes WebAssembly/JavaScript documentation and documents persistent browser storage options via OPFS. This suite does not claim SQLite comparison results until a reproducible workload and raw JSON output are added.
