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
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | noop_js_loop_1m | ok | 1 | 3 | 0.30 | 0.30 | 0.30 | 0.27 | Diagnostic: isolates JS loop overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | noop_js_loop_1m | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates JS loop overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | noop_worker_roundtrip_10k | ok | 1 | 3 | 183.20 | 191.70 | 191.70 | 185.23 | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | noop_worker_roundtrip_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_noop | ok | 1 | 3 | 181.40 | 188.60 | 188.60 | 182.53 | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_noop | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_small_payload | ok | 1 | 3 | 229.30 | 230.50 | 230.50 | 228.93 | Diagnostic: isolates structured clone overhead for a tiny binary payload. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_small_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a tiny binary payload. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_256b_payload | ok | 1 | 3 | 238.00 | 249.10 | 249.10 | 239.57 | Diagnostic: isolates structured clone overhead for a representative small value. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_256b_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates structured clone overhead for a representative small value. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_64kb_payload | ok | 1 | 3 | 14.70 | 15.40 | 15.40 | 14.60 | Diagnostic: isolates large binary structured clone overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_64kb_payload | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates large binary structured clone overhead. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | worker_binary_transfer_64kb | ok | 1 | 3 | 4.70 | 7.10 | 7.10 | 5.43 | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | worker_binary_transfer_64kb | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | noop_wasm_call_100k | ok | 1 | 3 | 3.30 | 3.60 | 3.60 | 3.30 | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | noop_wasm_call_100k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | encode_decode_10k_256b | ok | 1 | 3 | 13.00 | 13.20 | 13.20 | 13.03 | Diagnostic: isolates benchmark key/value generation and byte-copy cost. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | encode_decode_10k_256b | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates benchmark key/value generation and byte-copy cost. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | sdk_put_1k_single_calls | ok | 1 | 3 | 2088.50 | 2092.90 | 2092.90 | 2084.73 | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | sdk_put_1k_single_calls | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | sdk_bulk_put_10k | ok | 1 | 3 | 406.20 | 419.20 | 419.20 | 404.87 | Diagnostic: SDK bulk put path after data generation and empty DB setup. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | sdk_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: SDK bulk put path after data generation and empty DB setup. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | engine_stage_put_10k_rollback | ok | 1 | 3 | 28.60 | 30.70 | 30.70 | 28.57 | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | engine_stage_put_10k_rollback | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | engine_bulk_put_10k | ok | 1 | 3 | 409.90 | 427.10 | 427.10 | 413.47 | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | engine_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path. Not applicable to indexeddb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | indexeddb_bulk_put_10k | skipped | 0 | 0 | n/a | n/a | n/a | n/a | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. Not applicable to moyodb. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | indexeddb_bulk_put_10k | ok | 1 | 3 | 802.10 | 1185.00 | 1185.00 | 859.43 | Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | open_empty_db | ok | 1 | 3 | 34.30 | 35.50 | 35.50 | 33.97 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | open_empty_db | ok | 1 | 3 | 0.30 | 0.30 | 0.30 | 0.27 | Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | bulk_insert_10k | ok | 1 | 3 | 1335.60 | 1357.60 | 1357.60 | 1334.00 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | bulk_insert_10k | ok | 1 | 3 | 1768.80 | 1813.00 | 1813.00 | 1662.67 | Comparable batch insert workload. Test data and empty DB setup are outside the measured region. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | point_get_random_10k | ok | 1 | 3 | 1225.20 | 1277.60 | 1277.60 | 1237.83 | Random point reads after a 10k-row setup. This measures per-call SDK/Worker/WASM overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | point_get_random_10k | ok | 1 | 3 | 123.30 | 155.70 | 155.70 | 134.00 | Random point reads after a 10k-row setup. This measures per-call SDK/Worker/WASM overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | point_get_random_10k_bulk | ok | 1 | 3 | 155.20 | 200.30 | 200.30 | 168.77 | Bulk random point reads after a 10k-row setup. This measures the recommended public bulk read path. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | point_get_random_10k_bulk | ok | 1 | 3 | 138.30 | 141.40 | 141.40 | 139.00 | Bulk random point reads after a 10k-row setup. This measures the recommended public bulk read path. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | range_scan_100 | ok | 1 | 3 | 6.00 | 6.20 | 6.20 | 5.97 | Range scan over 100 rows. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | range_scan_100 | ok | 1 | 3 | 2.70 | 2.90 | 2.90 | 2.70 | Range scan over 100 rows. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | small_tx_1000_commits | ok | 1 | 3 | 4032.60 | 4380.30 | 4380.30 | 4124.30 | Small transaction commit overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | small_tx_1000_commits | ok | 1 | 3 | 959.60 | 1233.30 | 1233.30 | 1030.30 | Small transaction commit overhead. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | moyodb | worker_roundtrip_overhead | ok | 1 | 3 | 650.70 | 657.20 | 657.20 | 646.60 | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. |
| browser-bench-chromium-all-smoke.json | 2026-06-25T19:03:04.806Z | HeadlessChrome 149 | smoke | indexeddb | worker_roundtrip_overhead | skipped | 0 | 0 | n/a | n/a | n/a | n/a | MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable. Not applicable to indexeddb. |

## Reading this report

- Percentiles are computed from raw browser samples; warmups are excluded.
- Compare only rows with matching workload, browser, record count, key/value size, batch size, and transaction boundaries.
- Do not treat native Criterion results as browser SDK/OPFS performance.
- Commit the raw JSON alongside any published report so claims remain reproducible.
