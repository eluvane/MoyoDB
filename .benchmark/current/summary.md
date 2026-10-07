# MoyoDB benchmark baseline

Generated at: 2026-10-07T01:17:04.007Z
Mode: full
Node: v24.21.0
Platform: win32/x64

## Execution status

Full browser profile completed successfully: 31 workloads, 42 successful rows, 20 unsupported rows skipped, and no errors.
Content verification retained 100 non-null sample checksums; all 10 workloads shared by both engines have matching per-sample checksums.
All 27 native Criterion cases have complete samples and estimates. The 18 commit cases were repeated after the store-name fix. The nine BTree, WAL and recovery cases do not use the changed engine validation and retain this run's earlier measurements.
Rust validation: 257 tests passed, two ignored; one doc test passed. Chromium validation: 91 tests passed across the full run and one targeted retest, two opt-in tests skipped. Rustfmt passed.
Canonical Clippy remains blocked by existing argument-count and unsafe-block-comment errors outside the fix. Targeted native and WASM Clippy passed with the existing argument-count lint allowed.
The browser comparison against the earlier baseline has no matching settings: SDK build metadata and durability context differ. No browser regression conclusion is available.

## Policy

\- Native Rust Criterion timings are engine-core microbenchmarks.\
\- Browser timings include SDK, Worker, WASM and storage-backend overhead.\
\- Compare only matching workloads, browser profiles, sample counts and persistence modes.

## browser-sdk-wasm-opfs

| Case | ops/sec | avg ms | p50 ms | p95 ms | p99 ms | source |
| --- | --- | --- | --- | --- | --- | --- |
| moyodb / noop_js_loop_1m | 314.47 | 3.18 | 3.20 | 3.30 | 3.30 | browser-bench-chromium-all-full.json |
| moyodb / noop_worker_roundtrip_10k | 5.78 | 172.90 | 172.00 | 178.90 | 178.90 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_noop | 5.33 | 187.74 | 187.60 | 189.50 | 189.50 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_small_payload | 4.56 | 219.32 | 219.90 | 222.30 | 222.30 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_256b_payload | 4.61 | 216.76 | 214.40 | 226.30 | 226.30 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_64kb_payload | 70.22 | 14.24 | 13.10 | 17.30 | 17.30 | browser-bench-chromium-all-full.json |
| moyodb / worker_binary_transfer_64kb | 207.47 | 4.82 | 4.70 | 5.40 | 5.40 | browser-bench-chromium-all-full.json |
| moyodb / noop_wasm_call_100k | 200.00 | 5.00 | 5.10 | 5.20 | 5.20 | browser-bench-chromium-all-full.json |
| moyodb / encode_decode_10k_256b | 73.96 | 13.52 | 12.50 | 16.70 | 16.70 | browser-bench-chromium-all-full.json |
| moyodb / opfs_raw_write_100mb | 3.32 | 301.63 | 300.60 | 304.40 | 304.40 | browser-bench-chromium-all-full.json |
| moyodb / opfs_raw_read_random_10k | 1.03 | 973.67 | 968.50 | 1001.40 | 1001.40 | browser-bench-chromium-all-full.json |
| moyodb / sdk_put_1k_single_calls | 3.25 | 307.80 | 307.80 | 307.80 | 307.80 | browser-bench-chromium-all-full.json |
| moyodb / sdk_bulk_put_10k | 24.93 | 40.12 | 40.10 | 44.30 | 44.30 | browser-bench-chromium-all-full.json |
| moyodb / engine_stage_put_10k_rollback | 76.92 | 13.00 | 12.20 | 16.20 | 16.20 | browser-bench-chromium-all-full.json |
| moyodb / engine_bulk_put_10k | 29.47 | 33.93 | 32.10 | 39.10 | 39.10 | browser-bench-chromium-all-full.json |
| indexeddb / indexeddb_bulk_put_10k | 6.26 | 159.64 | 158.30 | 163.40 | 163.40 | browser-bench-chromium-all-full.json |
| moyodb / open_empty_db | 33.44 | 29.90 | 29.80 | 30.80 | 30.80 | browser-bench-chromium-all-full.json |
| indexeddb / open_empty_db | 1351.35 | 0.74 | 0.60 | 1.30 | 1.30 | browser-bench-chromium-all-full.json |
| moyodb / bulk_insert_10k | 22.19 | 45.06 | 44.50 | 52.50 | 52.50 | browser-bench-chromium-all-full.json |
| indexeddb / bulk_insert_10k | 6.50 | 153.90 | 153.00 | 160.10 | 160.10 | browser-bench-chromium-all-full.json |
| moyodb / point_get_random_10k | 2.98 | 335.54 | 336.00 | 353.70 | 353.70 | browser-bench-chromium-all-full.json |
| indexeddb / point_get_random_10k | 2.10 | 477.20 | 432.00 | 638.40 | 638.40 | browser-bench-chromium-all-full.json |
| moyodb / point_get_random_10k_pipelined | 14.62 | 68.38 | 63.70 | 78.90 | 78.90 | browser-bench-chromium-all-full.json |
| indexeddb / point_get_random_10k_pipelined | 7.35 | 136.10 | 137.50 | 139.60 | 139.60 | browser-bench-chromium-all-full.json |
| moyodb / point_get_random_10k_bulk | 91.07 | 10.98 | 7.20 | 18.10 | 18.10 | browser-bench-chromium-all-full.json |
| moyodb / range_scan_100 | 1063.83 | 0.94 | 1.00 | 1.10 | 1.10 | browser-bench-chromium-all-full.json |
| indexeddb / range_scan_100 | 961.54 | 1.04 | 0.80 | 2.10 | 2.10 | browser-bench-chromium-all-full.json |
| moyodb / reverse_scan_limit_1 | 1724.14 | 0.58 | 0.60 | 0.70 | 0.70 | browser-bench-chromium-all-full.json |
| indexeddb / reverse_scan_limit_1 | 3571.43 | 0.28 | 0.30 | 0.40 | 0.40 | browser-bench-chromium-all-full.json |
| moyodb / range_scan_1000 | 458.72 | 2.18 | 2.20 | 2.30 | 2.30 | browser-bench-chromium-all-full.json |
| indexeddb / range_scan_1000 | 184.50 | 5.42 | 5.40 | 5.50 | 5.50 | browser-bench-chromium-all-full.json |
| moyodb / range_scan_10000 | 71.94 | 13.90 | 14.00 | 14.30 | 14.30 | browser-bench-chromium-all-full.json |
| indexeddb / range_scan_10000 | 20.63 | 48.47 | 47.40 | 51.70 | 51.70 | browser-bench-chromium-all-full.json |
| moyodb / small_tx_1000_commits | 2.33 | 429.40 | 429.40 | 429.40 | 429.40 | browser-bench-chromium-all-full.json |
| indexeddb / small_tx_1000_commits | 9.48 | 105.50 | 105.50 | 105.50 | 105.50 | browser-bench-chromium-all-full.json |
| moyodb / large_value_64kb | 1.75 | 572.68 | 556.00 | 618.50 | 618.50 | browser-bench-chromium-all-full.json |
| indexeddb / large_value_64kb | 14.48 | 69.04 | 71.20 | 72.90 | 72.90 | browser-bench-chromium-all-full.json |
| moyodb / large_value_1mb | 1.52 | 657.70 | 644.30 | 740.60 | 740.60 | browser-bench-chromium-all-full.json |
| indexeddb / large_value_1mb | 13.67 | 73.17 | 69.80 | 81.10 | 81.10 | browser-bench-chromium-all-full.json |
| moyodb / recovery_after_dirty_close | 31.97 | 31.28 | 30.70 | 33.20 | 33.20 | browser-bench-chromium-all-full.json |
| moyodb / snapshot_export_import | 15.14 | 66.06 | 64.50 | 74.60 | 74.60 | browser-bench-chromium-all-full.json |
| moyodb / worker_roundtrip_overhead | 3.56 | 281.14 | 275.80 | 308.50 | 308.50 | browser-bench-chromium-all-full.json |


## Native Rust Criterion

| Case | Samples | Time estimate | 95% confidence interval |
| --- | --- | --- | --- |
| btree_10m/btree_insert_build_10m | 10 | 1.426 s | 1.409 s - 1.445 s |
| btree_10m/btree_point_get_10m_random_hot | 10 | 1.071 us | 944.365 ns - 1.202 us |
| btree_1m/btree_insert_build_1m | 10 | 114.307 ms | 113.358 ms - 115.893 ms |
| btree_1m/btree_point_get_1m_random_hot | 10 | 1.121 us | 619.387 ns - 1.817 us |
| btree_insert_build_1000 | 100 | 71.015 us | 70.790 us - 71.240 us |
| btree_point_get_1000 | 100 | 194.058 ns | 193.231 ns - 195.082 ns |
| btree_range_scan_1000 | 100 | 7.429 us | 7.380 us - 7.484 us |
| catalog_scaling/one_store_commit_feed/1 | 20 | 4.033 us | 4.011 us - 4.068 us |
| catalog_scaling/one_store_commit_feed/1024 | 20 | 65.116 us | 61.360 us - 68.824 us |
| catalog_scaling/one_store_commit_feed/128 | 20 | 23.583 us | 23.011 us - 24.045 us |
| catalog_scaling/one_store_commit_feed/8192 | 20 | 53.040 us | 51.956 us - 54.255 us |
| catalog_scaling/one_store_commit_live_reader/1 | 20 | 6.475 us | 6.443 us - 6.500 us |
| catalog_scaling/one_store_commit_live_reader/1024 | 20 | 79.656 us | 68.224 us - 99.102 us |
| catalog_scaling/one_store_commit_live_reader/128 | 20 | 25.195 us | 24.348 us - 25.872 us |
| catalog_scaling/one_store_commit_live_reader/8192 | 20 | 323.000 us | 314.592 us - 332.413 us |
| catalog_scaling/one_store_commit/1 | 20 | 3.101 us | 3.079 us - 3.127 us |
| catalog_scaling/one_store_commit/1024 | 20 | 45.397 us | 44.362 us - 46.560 us |
| catalog_scaling/one_store_commit/128 | 20 | 20.152 us | 19.605 us - 20.670 us |
| catalog_scaling/one_store_commit/8192 | 20 | 55.271 us | 48.907 us - 66.608 us |
| catalog_scaling/readonly_point/1 | 20 | 142.575 ns | 142.137 ns - 142.986 ns |
| catalog_scaling/readonly_point/1024 | 20 | 278.122 ns | 277.079 ns - 279.059 ns |
| catalog_scaling/readonly_point/128 | 20 | 285.447 ns | 283.938 ns - 286.882 ns |
| catalog_scaling/readonly_point/8192 | 20 | 159.084 ns | 158.089 ns - 160.961 ns |
| commit_hot_delete_4096_values_256b | 100 | 1.237 ms | 1.204 ms - 1.273 ms |
| commit_hot_update_4096_values_256b | 100 | 1.119 ms | 1.104 ms - 1.138 ms |
| recovery_replay_100_pages | 100 | 117.560 us | 114.391 us - 121.265 us |
| wal_append_100_pages | 100 | 34.971 us | 34.690 us - 35.388 us |

Raw Criterion JSON and HTML reports: `raw/rust/`. These native timings are separate from browser SDK timings.
