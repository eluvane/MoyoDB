# Browser benchmark report

Generated from 1 raw result file(s).

Native Rust engine microbench: not included here; run `cargo bench`.
WASM/Worker diagnostic benches: included for selected diagnostic workloads.
Browser SDK bench / OPFS persistence bench / IndexedDB comparison bench: included below when raw browser results exist.
Worker transport overhead bench: included for `worker_roundtrip_overhead` or `noop_worker_roundtrip_10k` when present.

## Environments

| File | Browser | Git SHA | SDK mode | WASM profile | IndexedDB durability | Backend | OPFS | SyncAccessHandle | Persistent context |
| ---- | ------- | ------- | -------- | ------------ | -------------------- | ------- | ---- | ---------------- | ------------------ |
| browser-bench-chromium-all-full.json | HeadlessChrome 153 | 7021fef9c0c668fe3246b0d1bf12a919b254d1ca-dirty | unknown | release | strict | OPFS SyncAccessHandle in a dedicated Worker | true | true | false |

## Results

| File | Generated | Browser | Profile | Engine | Workload | Status | warmups | n | p50 ms | p95 ms | p99 ms | mean ms | Notes |
| ---- | --------- | ------- | ------- | ------ | -------- | ------ | ------- | - | ------ | ------ | ------ | ------- | ----- |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | noop_js_loop_1m | ok | 1 | 5 | 3.10 | 3.30 | 3.30 | 3.12 | Diagnostic: isolates JS loop overhead. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | noop_js_loop_1m | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates JS loop overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | noop_worker_roundtrip_10k | ok | 1 | 5 | 181.20 | 193.40 | 193.40 | 182.90 | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | noop_worker_roundtrip_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_noop | ok | 1 | 5 | 181.10 | 184.50 | 184.50 | 180.92 | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_noop | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_small_payload | ok | 1 | 5 | 235.80 | 238.80 | 238.80 | 234.84 | Diagnostic: isolates structured clone overhead for a tiny binary payload. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_small_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a tiny binary payload. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_256b_payload | ok | 1 | 5 | 236.90 | 261.10 | 261.10 | 243.04 | Diagnostic: isolates structured clone overhead for a representative small value. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_256b_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a representative small value. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_64kb_payload | ok | 1 | 5 | 18.90 | 20.20 | 20.20 | 17.62 | Diagnostic: isolates large binary structured clone overhead. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_64kb_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates large binary structured clone overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | worker_binary_transfer_64kb | ok | 1 | 5 | 5.00 | 10.30 | 10.30 | 6.08 | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | worker_binary_transfer_64kb | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | noop_wasm_call_100k | ok | 0 | 3 | 5.90 | 7.70 | 7.70 | 6.17 | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | noop_wasm_call_100k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | encode_decode_10k_256b | ok | 1 | 5 | 14.30 | 18.90 | 18.90 | 15.34 | Diagnostic: isolates benchmark key/value generation and byte-copy cost. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | encode_decode_10k_256b | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates benchmark key/value generation and byte-copy cost. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | opfs_raw_write_100mb | ok | 0 | 3 | 336.40 | 337.70 | 337.70 | 336.53 | Diagnostic: OPFS raw sequential write throughput; no SDK, no WASM engine. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | opfs_raw_write_100mb | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: OPFS raw sequential write throughput; no SDK, no WASM engine. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | opfs_raw_read_random_10k | ok | 0 | 3 | 1181.00 | 1219.80 | 1219.80 | 1160.60 | Diagnostic: OPFS raw random-read cost; setup is outside the timed region. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | opfs_raw_read_random_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: OPFS raw random-read cost; setup is outside the timed region. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | sdk_put_1k_single_calls | ok | 0 | 1 | 654.00 | 654.00 | 654.00 | 654.00 | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | sdk_put_1k_single_calls | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | sdk_bulk_put_10k | ok | 1 | 5 | 68.10 | 78.40 | 78.40 | 70.94 | Diagnostic: SDK bulk put path after data generation and empty DB setup. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | sdk_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: SDK bulk put path after data generation and empty DB setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | engine_stage_put_10k_rollback | ok | 0 | 3 | 10.00 | 10.20 | 10.20 | 9.83 | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | engine_stage_put_10k_rollback | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | engine_bulk_put_10k | ok | 0 | 3 | 59.90 | 60.90 | 60.90 | 60.23 | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | engine_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | indexeddb_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. Not applicable to moyodb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | indexeddb_bulk_put_10k | ok | 1 | 5 | 158.80 | 161.40 | 161.40 | 158.84 | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | open_empty_db | ok | 1 | 5 | 32.50 | 36.10 | 36.10 | 32.84 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | open_empty_db | ok | 1 | 5 | 0.70 | 1.00 | 1.00 | 0.76 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | bulk_insert_10k | ok | 1 | 5 | 194.90 | 199.60 | 199.60 | 195.74 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | bulk_insert_10k | ok | 1 | 5 | 151.50 | 159.90 | 159.90 | 152.38 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | point_get_random_10k | ok | 1 | 5 | 855.30 | 908.50 | 908.50 | 866.74 | Sequential point-read latency after a 10k-row setup. Both engines keep exactly one request outstanding. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | point_get_random_10k | ok | 1 | 5 | 451.20 | 466.50 | 466.50 | 449.56 | Sequential point-read latency after a 10k-row setup. Both engines keep exactly one request outstanding. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | point_get_random_10k_pipelined | ok | 1 | 5 | 627.60 | 635.50 | 635.50 | 628.58 | Pipelined point-read throughput after a 10k-row setup. IndexedDB queues store.get requests; MoyoDB issues tx.get calls concurrently. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | point_get_random_10k_pipelined | ok | 1 | 5 | 123.10 | 125.70 | 125.70 | 122.58 | Pipelined point-read throughput after a 10k-row setup. IndexedDB queues store.get requests; MoyoDB issues tx.get calls concurrently. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | point_get_random_10k_bulk | ok | 1 | 5 | 507.80 | 510.20 | 510.20 | 506.00 | Bulk random point reads after a 10k-row setup. IndexedDB has no multi-key get, so compare this row with point_get_random_10k_pipelined for the IndexedDB best case. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | point_get_random_10k_bulk | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Bulk random point reads after a 10k-row setup. IndexedDB has no multi-key get, so compare this row with point_get_random_10k_pipelined for the IndexedDB best case. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | range_scan_100 | ok | 1 | 5 | 1.30 | 1.50 | 1.50 | 1.36 | Range scan over 100 rows. Both engines use their bulk range API: MoyoDB tx.scan, IndexedDB getAllKeys + getAll on the same range. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | range_scan_100 | ok | 1 | 5 | 1.00 | 1.00 | 1.00 | 0.90 | Range scan over 100 rows. Both engines use their bulk range API: MoyoDB tx.scan, IndexedDB getAllKeys + getAll on the same range. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | reverse_scan_limit_1 | ok | 1 | 5 | 0.60 | 0.70 | 0.70 | 0.60 | Reverse bounded scan: MoyoDB tx.scan({ reverse: true, limit: 1 }), IndexedDB openCursor(null, "prev") stopped after one row. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | reverse_scan_limit_1 | ok | 1 | 5 | 0.20 | 0.30 | 0.30 | 0.24 | Reverse bounded scan: MoyoDB tx.scan({ reverse: true, limit: 1 }), IndexedDB openCursor(null, "prev") stopped after one row. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | range_scan_1000 | ok | 1 | 5 | 6.10 | 6.80 | 6.80 | 6.22 | Range scan over 1,000 rows. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | range_scan_1000 | ok | 1 | 5 | 5.70 | 7.00 | 7.00 | 5.98 | Range scan over 1,000 rows. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | range_scan_10000 | ok | 1 | 3 | 52.00 | 52.80 | 52.80 | 52.20 | Range scan over 10,000 rows. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | range_scan_10000 | ok | 1 | 3 | 42.20 | 43.00 | 43.00 | 42.23 | Range scan over 10,000 rows. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | small_tx_1000_commits | ok | 0 | 1 | 485.40 | 485.40 | 485.40 | 485.40 | Small transaction commit overhead. One sample: each commit is its own OPFS flush. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | small_tx_1000_commits | ok | 0 | 1 | 118.10 | 118.10 | 118.10 | 118.10 | Small transaction commit overhead. One sample: each commit is its own OPFS flush. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | large_value_64kb | ok | 1 | 5 | 10158.10 | 10196.40 | 10196.40 | 9989.26 | Large values that exercise overflow/page paths. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | large_value_64kb | ok | 1 | 5 | 64.00 | 65.90 | 65.90 | 62.20 | Large values that exercise overflow/page paths. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | large_value_1mb | ok | 1 | 3 | 8451.70 | 9153.00 | 9153.00 | 8506.97 | Very large values; disabled in smoke profile. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | large_value_1mb | ok | 1 | 3 | 69.50 | 81.60 | 81.60 | 73.43 | Very large values; disabled in smoke profile. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | recovery_after_dirty_close | ok | 1 | 5 | 1136.50 | 1141.60 | 1141.60 | 1132.66 | Measures recovery after a simulated dirty close using an engine failpoint. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | recovery_after_dirty_close | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Measures recovery after a simulated dirty close using an engine failpoint. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | snapshot_export_import | ok | 1 | 5 | 241.50 | 255.40 | 255.40 | 243.06 | MoyoDB snapshot roundtrip; IndexedDB baseline is marked not applicable. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | snapshot_export_import | skipped | 0 | 0 | n/a | n/a | n/a | n/a | MoyoDB snapshot roundtrip; IndexedDB baseline is marked not applicable. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_overhead | ok | 1 | 5 | 282.60 | 295.80 | 295.80 | 281.84 | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. |
| browser-bench-chromium-all-full.json | 2026-09-24T09:56:32.447Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_overhead | skipped | 0 | 0 | n/a | n/a | n/a | n/a | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. Not applicable to indexeddb. |

## Content parity

Per-sample checksums of the data each engine read or wrote; rows must match to be comparable.

| File | Workload | Engines | Content |
| ---- | -------- | ------- | ------- |
| browser-bench-chromium-all-full.json | bulk_insert_10k | moyodb, indexeddb | match |
| browser-bench-chromium-all-full.json | point_get_random_10k | moyodb, indexeddb | match |
| browser-bench-chromium-all-full.json | point_get_random_10k_pipelined | moyodb, indexeddb | match |
| browser-bench-chromium-all-full.json | range_scan_100 | moyodb, indexeddb | match |
| browser-bench-chromium-all-full.json | reverse_scan_limit_1 | moyodb, indexeddb | match |
| browser-bench-chromium-all-full.json | range_scan_1000 | moyodb, indexeddb | match |
| browser-bench-chromium-all-full.json | range_scan_10000 | moyodb, indexeddb | match |
| browser-bench-chromium-all-full.json | small_tx_1000_commits | moyodb, indexeddb | match |
| browser-bench-chromium-all-full.json | large_value_64kb | moyodb, indexeddb | match |
| browser-bench-chromium-all-full.json | large_value_1mb | moyodb, indexeddb | match |

## Reading this report

- Percentiles are computed from raw browser samples; warmups are excluded.
- Compare only rows with matching workload, browser, record count, key/value size, batch size, and transaction boundaries.
- Random-read rows name their request mode: sequential, pipelined, or bulk. Compare rows of the same mode across engines.
- Rows without a Git SHA, with a WASM profile other than `release`, or without an IndexedDB durability value are not publishable numbers.
- With fewer than about 20 measured samples, p95/p99 say little about tail latency.
- Do not treat native Criterion results as browser SDK/OPFS performance.
- Commit the raw JSON alongside any published report so claims remain reproducible.
