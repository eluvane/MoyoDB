# MoyoDB benchmark baseline

Generated at: 2026-06-25T19:04:55.109Z
Mode: smoke
Node: v24.11.0
Platform: win32/x64

## Policy

- Native Rust Criterion timings are engine-core microbenchmarks.
- Browser timings include SDK, Worker, WASM and storage-backend overhead.
- Compare only matching workloads, browser profiles, sample counts and persistence modes.

## browser-sdk-wasm-opfs

| Case | ops/sec | avg ms | p50 ms | p95 ms | p99 ms | source |
| --- | --- | --- | --- | --- | --- | --- |
| moyodb / noop_js_loop_1m | 3750.00 | 0.27 | 0.30 | 0.30 | 0.30 | browser-bench-chromium-all-smoke.json |
| moyodb / noop_worker_roundtrip_10k | 5.40 | 185.23 | 183.20 | 191.70 | 191.70 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_noop | 5.48 | 182.53 | 181.40 | 188.60 | 188.60 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_small_payload | 4.37 | 228.93 | 229.30 | 230.50 | 230.50 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_256b_payload | 4.17 | 239.57 | 238.00 | 249.10 | 249.10 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_64kb_payload | 68.49 | 14.60 | 14.70 | 15.40 | 15.40 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_binary_transfer_64kb | 184.05 | 5.43 | 4.70 | 7.10 | 7.10 | browser-bench-chromium-all-smoke.json |
| moyodb / noop_wasm_call_100k | 303.03 | 3.30 | 3.30 | 3.60 | 3.60 | browser-bench-chromium-all-smoke.json |
| moyodb / encode_decode_10k_256b | 76.73 | 13.03 | 13.00 | 13.20 | 13.20 | browser-bench-chromium-all-smoke.json |
| moyodb / sdk_put_1k_single_calls | 0.48 | 2084.73 | 2088.50 | 2092.90 | 2092.90 | browser-bench-chromium-all-smoke.json |
| moyodb / sdk_bulk_put_10k | 2.47 | 404.87 | 406.20 | 419.20 | 419.20 | browser-bench-chromium-all-smoke.json |
| moyodb / engine_stage_put_10k_rollback | 35.01 | 28.57 | 28.60 | 30.70 | 30.70 | browser-bench-chromium-all-smoke.json |
| moyodb / engine_bulk_put_10k | 2.42 | 413.47 | 409.90 | 427.10 | 427.10 | browser-bench-chromium-all-smoke.json |
| indexeddb / indexeddb_bulk_put_10k | 1.16 | 859.43 | 802.10 | 1185.00 | 1185.00 | browser-bench-chromium-all-smoke.json |
| moyodb / open_empty_db | 29.44 | 33.97 | 34.30 | 35.50 | 35.50 | browser-bench-chromium-all-smoke.json |
| indexeddb / open_empty_db | 3750.00 | 0.27 | 0.30 | 0.30 | 0.30 | browser-bench-chromium-all-smoke.json |
| moyodb / bulk_insert_10k | 0.75 | 1334.00 | 1335.60 | 1357.60 | 1357.60 | browser-bench-chromium-all-smoke.json |
| indexeddb / bulk_insert_10k | 0.60 | 1662.67 | 1768.80 | 1813.00 | 1813.00 | browser-bench-chromium-all-smoke.json |
| moyodb / point_get_random_10k | 0.81 | 1237.83 | 1225.20 | 1277.60 | 1277.60 | browser-bench-chromium-all-smoke.json |
| indexeddb / point_get_random_10k | 7.46 | 134.00 | 123.30 | 155.70 | 155.70 | browser-bench-chromium-all-smoke.json |
| moyodb / point_get_random_10k_bulk | 5.93 | 168.77 | 155.20 | 200.30 | 200.30 | browser-bench-chromium-all-smoke.json |
| indexeddb / point_get_random_10k_bulk | 7.19 | 139.00 | 138.30 | 141.40 | 141.40 | browser-bench-chromium-all-smoke.json |
| moyodb / range_scan_100 | 167.60 | 5.97 | 6.00 | 6.20 | 6.20 | browser-bench-chromium-all-smoke.json |
| indexeddb / range_scan_100 | 370.37 | 2.70 | 2.70 | 2.90 | 2.90 | browser-bench-chromium-all-smoke.json |
| moyodb / small_tx_1000_commits | 0.24 | 4124.30 | 4032.60 | 4380.30 | 4380.30 | browser-bench-chromium-all-smoke.json |
| indexeddb / small_tx_1000_commits | 0.97 | 1030.30 | 959.60 | 1233.30 | 1233.30 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_overhead | 1.55 | 646.60 | 650.70 | 657.20 | 657.20 | browser-bench-chromium-all-smoke.json |
