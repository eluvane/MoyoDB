# Browser benchmark report

Generated from 1 raw result file(s).

Native Rust engine microbench: not included here; run `cargo bench`.
WASM/Worker diagnostic benches: included for selected diagnostic workloads.
Browser SDK bench / OPFS persistence bench / IndexedDB comparison bench: included below when raw browser results exist.
Worker transport overhead bench: included for `worker_roundtrip_overhead` or `noop_worker_roundtrip_10k` when present.

## Environments

| File | Browser | Git SHA | SDK mode | WASM profile | IndexedDB durability | Backend | OPFS | SyncAccessHandle | Persistent context |
| ---- | ------- | ------- | -------- | ------------ | -------------------- | ------- | ---- | ---------------- | ------------------ |
| browser-bench-chromium-all-smoke.json | HeadlessChrome 153 | 7021fef9c0c668fe3246b0d1bf12a919b254d1ca-dirty | unknown | release | strict | OPFS SyncAccessHandle in a dedicated Worker | true | true | false |

## Results

| File | Generated | Browser | Profile | Engine | Workload | Status | warmups | n | p50 ms | p95 ms | p99 ms | mean ms | Notes |
| ---- | --------- | ------- | ------- | ------ | -------- | ------ | ------- | - | ------ | ------ | ------ | ------- | ----- |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | noop_js_loop_1m | ok | 1 | 3 | 3.10 | 3.20 | 3.20 | 3.10 | Diagnostic: isolates JS loop overhead. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | noop_js_loop_1m | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates JS loop overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | noop_worker_roundtrip_10k | ok | 1 | 3 | 175.80 | 177.20 | 177.20 | 175.43 | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | noop_worker_roundtrip_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | worker_roundtrip_noop | ok | 1 | 3 | 172.50 | 177.90 | 177.90 | 174.20 | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | worker_roundtrip_noop | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | worker_roundtrip_small_payload | ok | 1 | 3 | 220.10 | 228.80 | 228.80 | 221.30 | Diagnostic: isolates structured clone overhead for a tiny binary payload. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | worker_roundtrip_small_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a tiny binary payload. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | worker_roundtrip_256b_payload | ok | 1 | 3 | 219.70 | 221.20 | 221.20 | 219.67 | Diagnostic: isolates structured clone overhead for a representative small value. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | worker_roundtrip_256b_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a representative small value. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | worker_roundtrip_64kb_payload | ok | 1 | 3 | 13.30 | 15.30 | 15.30 | 13.90 | Diagnostic: isolates large binary structured clone overhead. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | worker_roundtrip_64kb_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates large binary structured clone overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | worker_binary_transfer_64kb | ok | 1 | 3 | 4.80 | 7.70 | 7.70 | 5.67 | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | worker_binary_transfer_64kb | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | noop_wasm_call_100k | ok | 1 | 3 | 5.00 | 5.00 | 5.00 | 4.87 | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | noop_wasm_call_100k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | encode_decode_10k_256b | ok | 1 | 3 | 12.70 | 16.60 | 16.60 | 13.93 | Diagnostic: isolates benchmark key/value generation and byte-copy cost. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | encode_decode_10k_256b | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates benchmark key/value generation and byte-copy cost. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | sdk_put_1k_single_calls | ok | 1 | 3 | 514.50 | 522.20 | 522.20 | 513.83 | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | sdk_put_1k_single_calls | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | sdk_bulk_put_10k | ok | 1 | 3 | 68.00 | 69.40 | 69.40 | 68.03 | Diagnostic: SDK bulk put path after data generation and empty DB setup. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | sdk_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: SDK bulk put path after data generation and empty DB setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | engine_stage_put_10k_rollback | ok | 1 | 3 | 9.60 | 9.90 | 9.90 | 9.50 | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | engine_stage_put_10k_rollback | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | engine_bulk_put_10k | ok | 1 | 3 | 55.70 | 58.40 | 58.40 | 56.60 | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | engine_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | indexeddb_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. Not applicable to moyodb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | indexeddb_bulk_put_10k | ok | 1 | 3 | 156.20 | 157.10 | 157.10 | 156.13 | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | open_empty_db | ok | 1 | 3 | 31.40 | 32.20 | 32.20 | 30.43 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | open_empty_db | ok | 1 | 3 | 1.00 | 1.20 | 1.20 | 0.93 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | bulk_insert_10k | ok | 1 | 3 | 189.30 | 189.50 | 189.50 | 189.33 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | bulk_insert_10k | ok | 1 | 3 | 150.20 | 158.90 | 158.90 | 152.47 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | point_get_random_10k | ok | 1 | 3 | 825.10 | 835.60 | 835.60 | 826.30 | Sequential point-read latency after a 10k-row setup. Both engines keep exactly one request outstanding. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | point_get_random_10k | ok | 1 | 3 | 441.10 | 441.70 | 441.70 | 439.83 | Sequential point-read latency after a 10k-row setup. Both engines keep exactly one request outstanding. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | point_get_random_10k_pipelined | ok | 1 | 3 | 624.30 | 628.10 | 628.10 | 624.97 | Pipelined point-read throughput after a 10k-row setup. IndexedDB queues store.get requests; MoyoDB issues tx.get calls concurrently. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | point_get_random_10k_pipelined | ok | 1 | 3 | 122.40 | 126.90 | 126.90 | 123.10 | Pipelined point-read throughput after a 10k-row setup. IndexedDB queues store.get requests; MoyoDB issues tx.get calls concurrently. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | point_get_random_10k_bulk | ok | 1 | 3 | 502.00 | 508.40 | 508.40 | 503.43 | Bulk random point reads after a 10k-row setup. IndexedDB has no multi-key get, so compare this row with point_get_random_10k_pipelined for the IndexedDB best case. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | point_get_random_10k_bulk | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Bulk random point reads after a 10k-row setup. IndexedDB has no multi-key get, so compare this row with point_get_random_10k_pipelined for the IndexedDB best case. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | range_scan_100 | ok | 1 | 3 | 1.30 | 1.40 | 1.40 | 1.33 | Range scan over 100 rows. Both engines use their bulk range API: MoyoDB tx.scan, IndexedDB getAllKeys + getAll on the same range. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | range_scan_100 | ok | 1 | 3 | 0.70 | 0.80 | 0.80 | 0.73 | Range scan over 100 rows. Both engines use their bulk range API: MoyoDB tx.scan, IndexedDB getAllKeys + getAll on the same range. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | reverse_scan_limit_1 | ok | 1 | 3 | 0.60 | 0.70 | 0.70 | 0.57 | Reverse bounded scan: MoyoDB tx.scan({ reverse: true, limit: 1 }), IndexedDB openCursor(null, "prev") stopped after one row. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | reverse_scan_limit_1 | ok | 1 | 3 | 0.20 | 0.30 | 0.30 | 0.23 | Reverse bounded scan: MoyoDB tx.scan({ reverse: true, limit: 1 }), IndexedDB openCursor(null, "prev") stopped after one row. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | small_tx_1000_commits | ok | 1 | 3 | 507.30 | 513.50 | 513.50 | 504.57 | Small transaction commit overhead. One sample: each commit is its own OPFS flush. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | small_tx_1000_commits | ok | 1 | 3 | 99.70 | 113.60 | 113.60 | 103.67 | Small transaction commit overhead. One sample: each commit is its own OPFS flush. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | moyodb | worker_roundtrip_overhead | ok | 1 | 3 | 274.00 | 289.40 | 289.40 | 278.00 | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. |
| browser-bench-chromium-all-smoke.json | 2026-09-24T10:01:06.860Z | HeadlessChrome 153 | smoke | indexeddb | worker_roundtrip_overhead | skipped | 0 | 0 | n/a | n/a | n/a | n/a | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. Not applicable to indexeddb. |

## Content parity

Per-sample checksums of the data each engine read or wrote; rows must match to be comparable.

| File | Workload | Engines | Content |
| ---- | -------- | ------- | ------- |
| browser-bench-chromium-all-smoke.json | bulk_insert_10k | moyodb, indexeddb | match |
| browser-bench-chromium-all-smoke.json | point_get_random_10k | moyodb, indexeddb | match |
| browser-bench-chromium-all-smoke.json | point_get_random_10k_pipelined | moyodb, indexeddb | match |
| browser-bench-chromium-all-smoke.json | range_scan_100 | moyodb, indexeddb | match |
| browser-bench-chromium-all-smoke.json | reverse_scan_limit_1 | moyodb, indexeddb | match |
| browser-bench-chromium-all-smoke.json | small_tx_1000_commits | moyodb, indexeddb | match |

## Reading this report

- Percentiles are computed from raw browser samples; warmups are excluded.
- Compare only rows with matching workload, browser, record count, key/value size, batch size, and transaction boundaries.
- Random-read rows name their request mode: sequential, pipelined, or bulk. Compare rows of the same mode across engines.
- Rows without a Git SHA, with a WASM profile other than `release`, or without an IndexedDB durability value are not publishable numbers.
- With fewer than about 20 measured samples, p95/p99 say little about tail latency.
- Do not treat native Criterion results as browser SDK/OPFS performance.
- Commit the raw JSON alongside any published report so claims remain reproducible.
