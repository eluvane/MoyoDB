# Browser benchmark report

Generated from 1 raw result file(s).

Native Rust engine microbench: not included here; run `cargo bench`.
WASM/Worker diagnostic benches: included for selected diagnostic workloads.
Browser SDK bench / OPFS persistence bench / IndexedDB comparison bench: included below when raw browser results exist.
Worker transport overhead bench: included for `worker_roundtrip_overhead` or `noop_worker_roundtrip_10k` when present.

## Environments

| File | Browser | Git SHA | SDK mode | WASM profile | IndexedDB durability | Backend | OPFS | SyncAccessHandle | Persistent context |
| ---- | ------- | ------- | -------- | ------------ | -------------------- | ------- | ---- | ---------------- | ------------------ |
| browser-bench-chromium-all-full.json | HeadlessChrome 153 | 33ddfbfbf72f74443ce698010f083c78aa9931ea-dirty | development | release | strict | OPFS SyncAccessHandle in a dedicated Worker | true | true | false |

## Results

| File | Generated | Browser | Profile | Engine | Workload | Status | warmups | n | p50 ms | p95 ms | p99 ms | mean ms | Notes |
| ---- | --------- | ------- | ------- | ------ | -------- | ------ | ------- | - | ------ | ------ | ------ | ------- | ----- |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | noop_js_loop_1m | ok | 1 | 5 | 3.20 | 3.30 | 3.30 | 3.18 | Diagnostic: isolates JS loop overhead. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | noop_js_loop_1m | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates JS loop overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | noop_worker_roundtrip_10k | ok | 1 | 5 | 172.00 | 178.90 | 178.90 | 172.90 | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | noop_worker_roundtrip_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_noop | ok | 1 | 5 | 187.60 | 189.50 | 189.50 | 187.74 | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_noop | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_small_payload | ok | 1 | 5 | 219.90 | 222.30 | 222.30 | 219.32 | Diagnostic: isolates structured clone overhead for a tiny binary payload. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_small_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a tiny binary payload. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_256b_payload | ok | 1 | 5 | 214.40 | 226.30 | 226.30 | 216.76 | Diagnostic: isolates structured clone overhead for a representative small value. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_256b_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a representative small value. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_64kb_payload | ok | 1 | 5 | 13.10 | 17.30 | 17.30 | 14.24 | Diagnostic: isolates large binary structured clone overhead. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_64kb_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates large binary structured clone overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | worker_binary_transfer_64kb | ok | 1 | 5 | 4.70 | 5.40 | 5.40 | 4.82 | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | worker_binary_transfer_64kb | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | noop_wasm_call_100k | ok | 0 | 3 | 5.10 | 5.20 | 5.20 | 5.00 | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | noop_wasm_call_100k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | encode_decode_10k_256b | ok | 1 | 5 | 12.50 | 16.70 | 16.70 | 13.52 | Diagnostic: isolates benchmark key/value generation and byte-copy cost. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | encode_decode_10k_256b | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates benchmark key/value generation and byte-copy cost. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | opfs_raw_write_100mb | ok | 0 | 3 | 300.60 | 304.40 | 304.40 | 301.63 | Diagnostic: OPFS raw sequential write throughput; no SDK, no WASM engine. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | opfs_raw_write_100mb | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: OPFS raw sequential write throughput; no SDK, no WASM engine. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | opfs_raw_read_random_10k | ok | 0 | 3 | 968.50 | 1001.40 | 1001.40 | 973.67 | Diagnostic: OPFS raw random-read cost; setup is outside the timed region. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | opfs_raw_read_random_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: OPFS raw random-read cost; setup is outside the timed region. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | sdk_put_1k_single_calls | ok | 0 | 1 | 307.80 | 307.80 | 307.80 | 307.80 | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | sdk_put_1k_single_calls | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | sdk_bulk_put_10k | ok | 1 | 5 | 40.10 | 44.30 | 44.30 | 40.12 | Diagnostic: SDK bulk put path after data generation and empty DB setup. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | sdk_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: SDK bulk put path after data generation and empty DB setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | engine_stage_put_10k_rollback | ok | 0 | 3 | 12.20 | 16.20 | 16.20 | 13.00 | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | engine_stage_put_10k_rollback | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | engine_bulk_put_10k | ok | 0 | 3 | 32.10 | 39.10 | 39.10 | 33.93 | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | engine_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | indexeddb_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. Not applicable to moyodb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | indexeddb_bulk_put_10k | ok | 1 | 5 | 158.30 | 163.40 | 163.40 | 159.64 | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | open_empty_db | ok | 1 | 5 | 29.80 | 30.80 | 30.80 | 29.90 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | open_empty_db | ok | 1 | 5 | 0.60 | 1.30 | 1.30 | 0.74 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | bulk_insert_10k | ok | 1 | 5 | 44.50 | 52.50 | 52.50 | 45.06 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | bulk_insert_10k | ok | 1 | 5 | 153.00 | 160.10 | 160.10 | 153.90 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | point_get_random_10k | ok | 1 | 5 | 336.00 | 353.70 | 353.70 | 335.54 | Sequential point-read latency after a 10k-row setup. Both engines keep exactly one request outstanding. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | point_get_random_10k | ok | 1 | 5 | 432.00 | 638.40 | 638.40 | 477.20 | Sequential point-read latency after a 10k-row setup. Both engines keep exactly one request outstanding. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | point_get_random_10k_pipelined | ok | 1 | 5 | 63.70 | 78.90 | 78.90 | 68.38 | Pipelined point-read throughput after a 10k-row setup. IndexedDB queues store.get requests; MoyoDB issues tx.get calls concurrently. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | point_get_random_10k_pipelined | ok | 1 | 5 | 137.50 | 139.60 | 139.60 | 136.10 | Pipelined point-read throughput after a 10k-row setup. IndexedDB queues store.get requests; MoyoDB issues tx.get calls concurrently. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | point_get_random_10k_bulk | ok | 1 | 5 | 7.20 | 18.10 | 18.10 | 10.98 | Bulk random point reads after a 10k-row setup. IndexedDB has no multi-key get, so compare this row with point_get_random_10k_pipelined for the IndexedDB best case. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | point_get_random_10k_bulk | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Bulk random point reads after a 10k-row setup. IndexedDB has no multi-key get, so compare this row with point_get_random_10k_pipelined for the IndexedDB best case. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | range_scan_100 | ok | 1 | 5 | 1.00 | 1.10 | 1.10 | 0.94 | Range scan over 100 rows. Both engines use their bulk range API: MoyoDB tx.scan, IndexedDB getAllKeys + getAll on the same range. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | range_scan_100 | ok | 1 | 5 | 0.80 | 2.10 | 2.10 | 1.04 | Range scan over 100 rows. Both engines use their bulk range API: MoyoDB tx.scan, IndexedDB getAllKeys + getAll on the same range. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | reverse_scan_limit_1 | ok | 1 | 5 | 0.60 | 0.70 | 0.70 | 0.58 | Reverse bounded scan: MoyoDB tx.scan({ reverse: true, limit: 1 }), IndexedDB openCursor(null, "prev") stopped after one row. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | reverse_scan_limit_1 | ok | 1 | 5 | 0.30 | 0.40 | 0.40 | 0.28 | Reverse bounded scan: MoyoDB tx.scan({ reverse: true, limit: 1 }), IndexedDB openCursor(null, "prev") stopped after one row. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | range_scan_1000 | ok | 1 | 5 | 2.20 | 2.30 | 2.30 | 2.18 | Range scan over 1,000 rows. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | range_scan_1000 | ok | 1 | 5 | 5.40 | 5.50 | 5.50 | 5.42 | Range scan over 1,000 rows. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | range_scan_10000 | ok | 1 | 3 | 14.00 | 14.30 | 14.30 | 13.90 | Range scan over 10,000 rows. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | range_scan_10000 | ok | 1 | 3 | 47.40 | 51.70 | 51.70 | 48.47 | Range scan over 10,000 rows. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | small_tx_1000_commits | ok | 0 | 1 | 429.40 | 429.40 | 429.40 | 429.40 | Small transaction commit overhead. One sample: each commit is its own OPFS flush. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | small_tx_1000_commits | ok | 0 | 1 | 105.50 | 105.50 | 105.50 | 105.50 | Small transaction commit overhead. One sample: each commit is its own OPFS flush. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | large_value_64kb | ok | 1 | 5 | 556.00 | 618.50 | 618.50 | 572.68 | Large values that exercise overflow/page paths. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | large_value_64kb | ok | 1 | 5 | 71.20 | 72.90 | 72.90 | 69.04 | Large values that exercise overflow/page paths. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | large_value_1mb | ok | 1 | 3 | 644.30 | 740.60 | 740.60 | 657.70 | Very large values; disabled in smoke profile. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | large_value_1mb | ok | 1 | 3 | 69.80 | 81.10 | 81.10 | 73.17 | Very large values; disabled in smoke profile. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | recovery_after_dirty_close | ok | 1 | 5 | 30.70 | 33.20 | 33.20 | 31.28 | Measures recovery after a simulated dirty close using an engine failpoint. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | recovery_after_dirty_close | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Measures recovery after a simulated dirty close using an engine failpoint. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | snapshot_export_import | ok | 1 | 5 | 64.50 | 74.60 | 74.60 | 66.06 | MoyoDB snapshot roundtrip; IndexedDB baseline is marked not applicable. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | snapshot_export_import | skipped | 0 | 0 | n/a | n/a | n/a | n/a | MoyoDB snapshot roundtrip; IndexedDB baseline is marked not applicable. Not applicable to indexeddb. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | moyodb | worker_roundtrip_overhead | ok | 1 | 5 | 275.80 | 308.50 | 308.50 | 281.14 | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. |
| browser-bench-chromium-all-full.json | 2026-10-07T01:15:30.287Z | HeadlessChrome 153 | full | indexeddb | worker_roundtrip_overhead | skipped | 0 | 0 | n/a | n/a | n/a | n/a | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. Not applicable to indexeddb. |

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

\- Percentiles are computed from raw browser samples; warmups are excluded.\
\- Compare only rows with matching workload, browser, record count, key/value size, batch size, and transaction boundaries.\
\- Random-read rows name their request mode: sequential, pipelined, or bulk. Compare rows of the same mode across engines.\
\- Rows without a Git SHA, with a WASM profile other than `release`, or without an IndexedDB durability value are not publishable numbers.\
\- With fewer than about 20 measured samples, p95/p99 say little about tail latency.\
\- Do not treat native Criterion results as browser SDK/OPFS performance.\
\- Commit the raw JSON alongside any published report so claims remain reproducible.
