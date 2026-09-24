# MoyoDB benchmark baseline

Generated at: 2026-09-24T10:00:17.476Z
Mode: full
Node: v24.5.0
Platform: win32/x64

## Policy

- Native Rust Criterion timings are engine-core microbenchmarks.
- Browser timings include SDK, Worker, WASM and storage-backend overhead.
- Compare only matching workloads, browser profiles, sample counts and persistence modes.

## browser-sdk-wasm-opfs

| Case | ops/sec | avg ms | p50 ms | p95 ms | p99 ms | source |
| --- | --- | --- | --- | --- | --- | --- |
| moyodb / noop_js_loop_1m | 320.51 | 3.12 | 3.10 | 3.30 | 3.30 | browser-bench-chromium-all-full.json |
| moyodb / noop_worker_roundtrip_10k | 5.47 | 182.90 | 181.20 | 193.40 | 193.40 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_noop | 5.53 | 180.92 | 181.10 | 184.50 | 184.50 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_small_payload | 4.26 | 234.84 | 235.80 | 238.80 | 238.80 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_256b_payload | 4.11 | 243.04 | 236.90 | 261.10 | 261.10 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_64kb_payload | 56.75 | 17.62 | 18.90 | 20.20 | 20.20 | browser-bench-chromium-all-full.json |
| moyodb / worker_binary_transfer_64kb | 164.47 | 6.08 | 5.00 | 10.30 | 10.30 | browser-bench-chromium-all-full.json |
| moyodb / noop_wasm_call_100k | 162.16 | 6.17 | 5.90 | 7.70 | 7.70 | browser-bench-chromium-all-full.json |
| moyodb / encode_decode_10k_256b | 65.19 | 15.34 | 14.30 | 18.90 | 18.90 | browser-bench-chromium-all-full.json |
| moyodb / opfs_raw_write_100mb | 2.97 | 336.53 | 336.40 | 337.70 | 337.70 | browser-bench-chromium-all-full.json |
| moyodb / opfs_raw_read_random_10k | 0.86 | 1160.60 | 1181.00 | 1219.80 | 1219.80 | browser-bench-chromium-all-full.json |
| moyodb / sdk_put_1k_single_calls | 1.53 | 654.00 | 654.00 | 654.00 | 654.00 | browser-bench-chromium-all-full.json |
| moyodb / sdk_bulk_put_10k | 14.10 | 70.94 | 68.10 | 78.40 | 78.40 | browser-bench-chromium-all-full.json |
| moyodb / engine_stage_put_10k_rollback | 101.69 | 9.83 | 10.00 | 10.20 | 10.20 | browser-bench-chromium-all-full.json |
| moyodb / engine_bulk_put_10k | 16.60 | 60.23 | 59.90 | 60.90 | 60.90 | browser-bench-chromium-all-full.json |
| indexeddb / indexeddb_bulk_put_10k | 6.30 | 158.84 | 158.80 | 161.40 | 161.40 | browser-bench-chromium-all-full.json |
| moyodb / open_empty_db | 30.45 | 32.84 | 32.50 | 36.10 | 36.10 | browser-bench-chromium-all-full.json |
| indexeddb / open_empty_db | 1315.79 | 0.76 | 0.70 | 1.00 | 1.00 | browser-bench-chromium-all-full.json |
| moyodb / bulk_insert_10k | 5.11 | 195.74 | 194.90 | 199.60 | 199.60 | browser-bench-chromium-all-full.json |
| indexeddb / bulk_insert_10k | 6.56 | 152.38 | 151.50 | 159.90 | 159.90 | browser-bench-chromium-all-full.json |
| moyodb / point_get_random_10k | 1.15 | 866.74 | 855.30 | 908.50 | 908.50 | browser-bench-chromium-all-full.json |
| indexeddb / point_get_random_10k | 2.22 | 449.56 | 451.20 | 466.50 | 466.50 | browser-bench-chromium-all-full.json |
| moyodb / point_get_random_10k_pipelined | 1.59 | 628.58 | 627.60 | 635.50 | 635.50 | browser-bench-chromium-all-full.json |
| indexeddb / point_get_random_10k_pipelined | 8.16 | 122.58 | 123.10 | 125.70 | 125.70 | browser-bench-chromium-all-full.json |
| moyodb / point_get_random_10k_bulk | 1.98 | 506.00 | 507.80 | 510.20 | 510.20 | browser-bench-chromium-all-full.json |
| moyodb / range_scan_100 | 735.29 | 1.36 | 1.30 | 1.50 | 1.50 | browser-bench-chromium-all-full.json |
| indexeddb / range_scan_100 | 1111.11 | 0.90 | 1.00 | 1.00 | 1.00 | browser-bench-chromium-all-full.json |
| moyodb / reverse_scan_limit_1 | 1666.67 | 0.60 | 0.60 | 0.70 | 0.70 | browser-bench-chromium-all-full.json |
| indexeddb / reverse_scan_limit_1 | 4166.67 | 0.24 | 0.20 | 0.30 | 0.30 | browser-bench-chromium-all-full.json |
| moyodb / range_scan_1000 | 160.77 | 6.22 | 6.10 | 6.80 | 6.80 | browser-bench-chromium-all-full.json |
| indexeddb / range_scan_1000 | 167.22 | 5.98 | 5.70 | 7.00 | 7.00 | browser-bench-chromium-all-full.json |
| moyodb / range_scan_10000 | 19.16 | 52.20 | 52.00 | 52.80 | 52.80 | browser-bench-chromium-all-full.json |
| indexeddb / range_scan_10000 | 23.68 | 42.23 | 42.20 | 43.00 | 43.00 | browser-bench-chromium-all-full.json |
| moyodb / small_tx_1000_commits | 2.06 | 485.40 | 485.40 | 485.40 | 485.40 | browser-bench-chromium-all-full.json |
| indexeddb / small_tx_1000_commits | 8.47 | 118.10 | 118.10 | 118.10 | 118.10 | browser-bench-chromium-all-full.json |
| moyodb / large_value_64kb | 0.10 | 9989.26 | 10158.10 | 10196.40 | 10196.40 | browser-bench-chromium-all-full.json |
| indexeddb / large_value_64kb | 16.08 | 62.20 | 64.00 | 65.90 | 65.90 | browser-bench-chromium-all-full.json |
| moyodb / large_value_1mb | 0.12 | 8506.97 | 8451.70 | 9153.00 | 9153.00 | browser-bench-chromium-all-full.json |
| indexeddb / large_value_1mb | 13.62 | 73.43 | 69.50 | 81.60 | 81.60 | browser-bench-chromium-all-full.json |
| moyodb / recovery_after_dirty_close | 0.88 | 1132.66 | 1136.50 | 1141.60 | 1141.60 | browser-bench-chromium-all-full.json |
| moyodb / snapshot_export_import | 4.11 | 243.06 | 241.50 | 255.40 | 255.40 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_overhead | 3.55 | 281.84 | 282.60 | 295.80 | 295.80 | browser-bench-chromium-all-full.json |
