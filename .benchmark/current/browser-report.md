# Browser benchmark report

Generated from 1 raw result file(s).

Native Rust engine microbench: not included here; run `cargo bench`.
WASM/Worker diagnostic benches: included for selected diagnostic workloads.
Browser SDK bench / OPFS persistence bench / IndexedDB comparison bench: included below when raw browser results exist.
Worker transport overhead bench: included for `worker_roundtrip_overhead` or `noop_worker_roundtrip_10k` when present.

## Environments

| File | Browser | SDK mode | WASM mode | Backend | OPFS | SyncAccessHandle | Persistent context |
| ---- | ------- | -------- | --------- | ------- | ---- | ---------------- | ------------------ |
| browser-bench-chromium-all-smoke.json | HeadlessChrome 149 | unknown | not introspected at runtime; use npm run build:wasm:release before publishing benchmark numbers | OPFS SyncAccessHandle in a dedicated Worker | true | true | false |

## Results

| File | Generated | Browser | Profile | Engine | Workload | Status | warmups | n | p50 ms | p95 ms | p99 ms | mean ms | Notes |
| ---- | --------- | ------- | ------- | ------ | -------- | ------ | ------- | - | ------ | ------ | ------ | ------- | ----- |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | noop_js_loop_1m | ok | 1 | 3 | 0.20 | 0.30 | 0.30 | 0.23 | Diagnostic: isolates JS loop overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | noop_js_loop_1m | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates JS loop overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | noop_worker_roundtrip_10k | ok | 1 | 3 | 197.10 | 205.40 | 205.40 | 196.40 | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | noop_worker_roundtrip_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_noop | ok | 1 | 3 | 187.10 | 189.60 | 189.60 | 186.57 | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_noop | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_small_payload | ok | 1 | 3 | 243.80 | 256.00 | 256.00 | 246.60 | Diagnostic: isolates structured clone overhead for a tiny binary payload. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_small_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a tiny binary payload. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_256b_payload | ok | 1 | 3 | 436.10 | 484.60 | 484.60 | 449.83 | Diagnostic: isolates structured clone overhead for a representative small value. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_256b_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a representative small value. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_64kb_payload | ok | 1 | 3 | 27.50 | 29.40 | 29.40 | 26.43 | Diagnostic: isolates large binary structured clone overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_64kb_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates large binary structured clone overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | worker_binary_transfer_64kb | ok | 1 | 3 | 11.20 | 13.40 | 13.40 | 11.73 | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | worker_binary_transfer_64kb | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | noop_wasm_call_100k | ok | 1 | 3 | 5.80 | 5.80 | 5.80 | 5.80 | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | noop_wasm_call_100k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | encode_decode_10k_256b | ok | 1 | 3 | 25.50 | 26.20 | 26.20 | 25.70 | Diagnostic: isolates benchmark key/value generation and byte-copy cost. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | encode_decode_10k_256b | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates benchmark key/value generation and byte-copy cost. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | sdk_put_1k_single_calls | ok | 1 | 3 | 4114.30 | 4147.80 | 4147.80 | 4110.93 | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | sdk_put_1k_single_calls | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | sdk_bulk_put_10k | ok | 1 | 3 | 817.50 | 833.30 | 833.30 | 822.13 | Diagnostic: SDK bulk put path after data generation and empty DB setup. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | sdk_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: SDK bulk put path after data generation and empty DB setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | engine_stage_put_10k_rollback | ok | 1 | 3 | 48.50 | 50.90 | 50.90 | 47.37 | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | engine_stage_put_10k_rollback | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | engine_bulk_put_10k | ok | 1 | 3 | 775.40 | 783.70 | 783.70 | 775.30 | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | engine_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | indexeddb_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. Not applicable to moyodb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | indexeddb_bulk_put_10k | ok | 1 | 3 | 1890.10 | 2653.50 | 2653.50 | 1887.40 | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | open_empty_db | ok | 1 | 3 | 48.00 | 51.80 | 51.80 | 48.77 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | open_empty_db | ok | 1 | 3 | 0.40 | 0.40 | 0.40 | 0.37 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | bulk_insert_10k | ok | 1 | 3 | 2788.10 | 2869.60 | 2869.60 | 2811.63 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | bulk_insert_10k | ok | 1 | 3 | 3433.40 | 3566.10 | 3566.10 | 3217.20 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | point_get_random_10k | ok | 1 | 3 | 2426.30 | 2428.70 | 2428.70 | 2359.37 | Random point reads after a 10k-row setup. This measures per-call SDK/Worker/WASM overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | point_get_random_10k | ok | 1 | 3 | 292.80 | 298.50 | 298.50 | 293.07 | Random point reads after a 10k-row setup. This measures per-call SDK/Worker/WASM overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | point_get_random_10k_bulk | ok | 1 | 3 | 277.80 | 281.00 | 281.00 | 271.70 | Bulk random point reads after a 10k-row setup. This measures the recommended public bulk read path. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | point_get_random_10k_bulk | ok | 1 | 3 | 381.20 | 381.70 | 381.70 | 360.30 | Bulk random point reads after a 10k-row setup. This measures the recommended public bulk read path. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | range_scan_100 | ok | 1 | 3 | 9.80 | 12.60 | 12.60 | 10.67 | Range scan over 100 rows. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | range_scan_100 | ok | 1 | 3 | 2.60 | 3.00 | 3.00 | 2.70 | Range scan over 100 rows. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | small_tx_1000_commits | ok | 1 | 3 | 2940.30 | 4084.60 | 4084.60 | 3240.13 | Small transaction commit overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | small_tx_1000_commits | ok | 1 | 3 | 974.30 | 1198.60 | 1198.60 | 1040.33 | Small transaction commit overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_overhead | ok | 1 | 3 | 655.40 | 658.70 | 658.70 | 634.10 | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T18:59:49.777Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_overhead | skipped | 0 | 0 | n/a | n/a | n/a | n/a | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. Not applicable to indexeddb. |

## Reading this report

- Percentiles are computed from raw browser samples; warmups are excluded.
- Compare only rows with matching workload, browser, record count, key/value size, batch size, and transaction boundaries.
- Do not treat native Criterion results as browser SDK/OPFS performance.
- Commit the raw JSON alongside any published report so claims remain reproducible.
