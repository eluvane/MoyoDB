# MoyoDB benchmark baseline

Generated at: 2026-06-25T19:02:43.842Z
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
| moyodb / noop_js_loop_1m | 4285.71 | 0.23 | 0.20 | 0.30 | 0.30 | browser-bench-chromium-all-smoke.json |
| moyodb / noop_worker_roundtrip_10k | 5.09 | 196.40 | 197.10 | 205.40 | 205.40 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_noop | 5.36 | 186.57 | 187.10 | 189.60 | 189.60 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_small_payload | 4.06 | 246.60 | 243.80 | 256.00 | 256.00 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_256b_payload | 2.22 | 449.83 | 436.10 | 484.60 | 484.60 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_64kb_payload | 37.83 | 26.43 | 27.50 | 29.40 | 29.40 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_binary_transfer_64kb | 85.23 | 11.73 | 11.20 | 13.40 | 13.40 | browser-bench-chromium-all-smoke.json |
| moyodb / noop_wasm_call_100k | 172.41 | 5.80 | 5.80 | 5.80 | 5.80 | browser-bench-chromium-all-smoke.json |
| moyodb / encode_decode_10k_256b | 38.91 | 25.70 | 25.50 | 26.20 | 26.20 | browser-bench-chromium-all-smoke.json |
| moyodb / sdk_put_1k_single_calls | 0.24 | 4110.93 | 4114.30 | 4147.80 | 4147.80 | browser-bench-chromium-all-smoke.json |
| moyodb / sdk_bulk_put_10k | 1.22 | 822.13 | 817.50 | 833.30 | 833.30 | browser-bench-chromium-all-smoke.json |
| moyodb / engine_stage_put_10k_rollback | 21.11 | 47.37 | 48.50 | 50.90 | 50.90 | browser-bench-chromium-all-smoke.json |
| moyodb / engine_bulk_put_10k | 1.29 | 775.30 | 775.40 | 783.70 | 783.70 | browser-bench-chromium-all-smoke.json |
| indexeddb / indexeddb_bulk_put_10k | 0.53 | 1887.40 | 1890.10 | 2653.50 | 2653.50 | browser-bench-chromium-all-smoke.json |
| moyodb / open_empty_db | 20.51 | 48.77 | 48.00 | 51.80 | 51.80 | browser-bench-chromium-all-smoke.json |
| indexeddb / open_empty_db | 2727.27 | 0.37 | 0.40 | 0.40 | 0.40 | browser-bench-chromium-all-smoke.json |
| moyodb / bulk_insert_10k | 0.36 | 2811.63 | 2788.10 | 2869.60 | 2869.60 | browser-bench-chromium-all-smoke.json |
| indexeddb / bulk_insert_10k | 0.31 | 3217.20 | 3433.40 | 3566.10 | 3566.10 | browser-bench-chromium-all-smoke.json |
| moyodb / point_get_random_10k | 0.42 | 2359.37 | 2426.30 | 2428.70 | 2428.70 | browser-bench-chromium-all-smoke.json |
| indexeddb / point_get_random_10k | 3.41 | 293.07 | 292.80 | 298.50 | 298.50 | browser-bench-chromium-all-smoke.json |
| moyodb / point_get_random_10k_bulk | 3.68 | 271.70 | 277.80 | 281.00 | 281.00 | browser-bench-chromium-all-smoke.json |
| indexeddb / point_get_random_10k_bulk | 2.78 | 360.30 | 381.20 | 381.70 | 381.70 | browser-bench-chromium-all-smoke.json |
| moyodb / range_scan_100 | 93.75 | 10.67 | 9.80 | 12.60 | 12.60 | browser-bench-chromium-all-smoke.json |
| indexeddb / range_scan_100 | 370.37 | 2.70 | 2.60 | 3.00 | 3.00 | browser-bench-chromium-all-smoke.json |
| moyodb / small_tx_1000_commits | 0.31 | 3240.13 | 2940.30 | 4084.60 | 4084.60 | browser-bench-chromium-all-smoke.json |
| indexeddb / small_tx_1000_commits | 0.96 | 1040.33 | 974.30 | 1198.60 | 1198.60 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_overhead | 1.58 | 634.10 | 655.40 | 658.70 | 658.70 | browser-bench-chromium-all-smoke.json |
