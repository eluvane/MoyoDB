# MoyoDB benchmark baseline

Generated at: 2026-09-24T10:01:49.723Z
Mode: smoke
Node: v24.5.0
Platform: win32/x64

## Policy

- Native Rust Criterion timings are engine-core microbenchmarks.
- Browser timings include SDK, Worker, WASM and storage-backend overhead.
- Compare only matching workloads, browser profiles, sample counts and persistence modes.

## browser-sdk-wasm-opfs

| Case | ops/sec | avg ms | p50 ms | p95 ms | p99 ms | source |
| --- | --- | --- | --- | --- | --- | --- |
| moyodb / noop_js_loop_1m | 322.58 | 3.10 | 3.10 | 3.20 | 3.20 | browser-bench-chromium-all-smoke.json |
| moyodb / noop_worker_roundtrip_10k | 5.70 | 175.43 | 175.80 | 177.20 | 177.20 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_noop | 5.74 | 174.20 | 172.50 | 177.90 | 177.90 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_small_payload | 4.52 | 221.30 | 220.10 | 228.80 | 228.80 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_256b_payload | 4.55 | 219.67 | 219.70 | 221.20 | 221.20 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_64kb_payload | 71.94 | 13.90 | 13.30 | 15.30 | 15.30 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_binary_transfer_64kb | 176.47 | 5.67 | 4.80 | 7.70 | 7.70 | browser-bench-chromium-all-smoke.json |
| moyodb / noop_wasm_call_100k | 205.48 | 4.87 | 5.00 | 5.00 | 5.00 | browser-bench-chromium-all-smoke.json |
| moyodb / encode_decode_10k_256b | 71.77 | 13.93 | 12.70 | 16.60 | 16.60 | browser-bench-chromium-all-smoke.json |
| moyodb / sdk_put_1k_single_calls | 1.95 | 513.83 | 514.50 | 522.20 | 522.20 | browser-bench-chromium-all-smoke.json |
| moyodb / sdk_bulk_put_10k | 14.70 | 68.03 | 68.00 | 69.40 | 69.40 | browser-bench-chromium-all-smoke.json |
| moyodb / engine_stage_put_10k_rollback | 105.26 | 9.50 | 9.60 | 9.90 | 9.90 | browser-bench-chromium-all-smoke.json |
| moyodb / engine_bulk_put_10k | 17.67 | 56.60 | 55.70 | 58.40 | 58.40 | browser-bench-chromium-all-smoke.json |
| indexeddb / indexeddb_bulk_put_10k | 6.40 | 156.13 | 156.20 | 157.10 | 157.10 | browser-bench-chromium-all-smoke.json |
| moyodb / open_empty_db | 32.86 | 30.43 | 31.40 | 32.20 | 32.20 | browser-bench-chromium-all-smoke.json |
| indexeddb / open_empty_db | 1071.43 | 0.93 | 1.00 | 1.20 | 1.20 | browser-bench-chromium-all-smoke.json |
| moyodb / bulk_insert_10k | 5.28 | 189.33 | 189.30 | 189.50 | 189.50 | browser-bench-chromium-all-smoke.json |
| indexeddb / bulk_insert_10k | 6.56 | 152.47 | 150.20 | 158.90 | 158.90 | browser-bench-chromium-all-smoke.json |
| moyodb / point_get_random_10k | 1.21 | 826.30 | 825.10 | 835.60 | 835.60 | browser-bench-chromium-all-smoke.json |
| indexeddb / point_get_random_10k | 2.27 | 439.83 | 441.10 | 441.70 | 441.70 | browser-bench-chromium-all-smoke.json |
| moyodb / point_get_random_10k_pipelined | 1.60 | 624.97 | 624.30 | 628.10 | 628.10 | browser-bench-chromium-all-smoke.json |
| indexeddb / point_get_random_10k_pipelined | 8.12 | 123.10 | 122.40 | 126.90 | 126.90 | browser-bench-chromium-all-smoke.json |
| moyodb / point_get_random_10k_bulk | 1.99 | 503.43 | 502.00 | 508.40 | 508.40 | browser-bench-chromium-all-smoke.json |
| moyodb / range_scan_100 | 750.00 | 1.33 | 1.30 | 1.40 | 1.40 | browser-bench-chromium-all-smoke.json |
| indexeddb / range_scan_100 | 1363.64 | 0.73 | 0.70 | 0.80 | 0.80 | browser-bench-chromium-all-smoke.json |
| moyodb / reverse_scan_limit_1 | 1764.71 | 0.57 | 0.60 | 0.70 | 0.70 | browser-bench-chromium-all-smoke.json |
| indexeddb / reverse_scan_limit_1 | 4285.71 | 0.23 | 0.20 | 0.30 | 0.30 | browser-bench-chromium-all-smoke.json |
| moyodb / small_tx_1000_commits | 1.98 | 504.57 | 507.30 | 513.50 | 513.50 | browser-bench-chromium-all-smoke.json |
| indexeddb / small_tx_1000_commits | 9.65 | 103.67 | 99.70 | 113.60 | 113.60 | browser-bench-chromium-all-smoke.json |
| moyodb / worker_roundtrip_overhead | 3.60 | 278.00 | 274.00 | 289.40 | 289.40 | browser-bench-chromium-all-smoke.json |
