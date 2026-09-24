# MoyoDB benchmark comparison

Current baseline: 2026-09-24T10:00:17.476Z
Previous baseline: 2026-09-22T07:32:50.515Z
Thresholds: throughput -5%, avg/p95 +5%, p99 +8%

Detected 9 benchmark governance regression(s).

## browser-sdk-wasm-opfs

| Case | prev ops/sec | curr ops/sec | Δ ops | Δ avg | Δ p95 | Δ p99 | status |
| --- | --- | --- | --- | --- | --- | --- | --- |
| moyodb / noop_js_loop_1m | 250.00 | 320.51 | +28.21% | -22.00% | -31.25% | -31.25% | ok |
| moyodb / noop_worker_roundtrip_10k | 5.28 | 5.47 | +3.61% | -3.48% | -6.66% | -6.66% | ok |
| moyodb / worker_roundtrip_noop | 5.38 | 5.53 | +2.72% | -2.65% | -5.53% | -5.53% | ok |
| moyodb / worker_roundtrip_small_payload | 3.11 | 4.26 | +36.80% | -26.90% | -37.86% | -37.86% | ok |
| moyodb / worker_roundtrip_256b_payload | 3.80 | 4.11 | +8.16% | -7.54% | -16.79% | -16.79% | ok |
| moyodb / worker_roundtrip_64kb_payload | 55.93 | 56.75 | +1.48% | -1.45% | -0.00% | -0.00% | ok |
| moyodb / worker_binary_transfer_64kb | 168.35 | 164.47 | -2.30% | +2.36% | +43.06% | +43.06% | p95 latency +43.06%; p99 latency +43.06% |
| moyodb / noop_wasm_call_100k | 157.07 | 162.16 | +3.24% | -3.14% | -9.41% | -9.41% | ok |
| moyodb / encode_decode_10k_256b | 72.25 | 65.19 | -9.78% | +10.84% | +5.59% | +5.59% | throughput -9.78%; avg latency +10.84%; p95 latency +5.59% |
| moyodb / opfs_raw_write_100mb | 2.78 | 2.97 | +6.74% | -6.31% | -16.80% | -16.80% | ok |
| moyodb / opfs_raw_read_random_10k | 0.95 | 0.86 | -9.33% | +10.29% | +11.22% | +11.22% | throughput -9.33%; avg latency +10.29%; p95 latency +11.22%; p99 latency +11.22% |
| moyodb / sdk_put_1k_single_calls | 0.10 | 1.53 | +1424.51% | -93.44% | -93.44% | -93.44% | ok |
| moyodb / sdk_bulk_put_10k | 10.01 | 14.10 | +40.80% | -28.97% | -25.48% | -25.48% | ok |
| moyodb / engine_stage_put_10k_rollback | 41.10 | 101.69 | +147.46% | -59.59% | -58.54% | -58.54% | ok |
| moyodb / engine_bulk_put_10k | 11.27 | 16.60 | +47.26% | -32.09% | -32.78% | -32.78% | ok |
| indexeddb / indexeddb_bulk_put_10k | 6.18 | 6.30 | +1.88% | -1.84% | -3.06% | -3.06% | ok |
| moyodb / open_empty_db | 31.99 | 30.45 | -4.81% | +5.05% | +8.73% | +8.73% | avg latency +5.05%; p95 latency +8.73%; p99 latency +8.73% |
| indexeddb / open_empty_db | 1562.50 | 1315.79 | -15.79% | +18.75% | +25.00% | +25.00% | throughput -15.79%; avg latency +18.75%; p95 latency +25.00%; p99 latency +25.00% |
| moyodb / bulk_insert_10k | 2.19 | 5.11 | +133.45% | -57.16% | -57.01% | -57.01% | ok |
| indexeddb / bulk_insert_10k | 6.47 | 6.56 | +1.37% | -1.35% | -7.41% | -7.41% | ok |
| moyodb / point_get_random_10k | 1.09 | 1.15 | +5.98% | -5.65% | -3.10% | -3.10% | ok |
| indexeddb / point_get_random_10k | 8.22 | 2.22 | -72.93% | +269.46% | +269.65% | +269.65% | throughput -72.93%; avg latency +269.46%; p95 latency +269.65%; p99 latency +269.65% |
| moyodb / point_get_random_10k_bulk | 1.69 | 1.98 | +16.75% | -14.35% | -15.46% | -15.46% | ok |
| moyodb / range_scan_100 | 555.56 | 735.29 | +32.35% | -24.44% | -25.00% | -25.00% | ok |
| indexeddb / range_scan_100 | 769.23 | 1111.11 | +44.44% | -30.77% | -28.57% | -28.57% | ok |
| moyodb / range_scan_1000 | 129.20 | 160.77 | +24.44% | -19.64% | -16.05% | -16.05% | ok |
| indexeddb / range_scan_1000 | 156.25 | 167.22 | +7.02% | -6.56% | +6.06% | +6.06% | p95 latency +6.06% |
| moyodb / range_scan_10000 | 17.06 | 19.16 | +12.26% | -10.92% | -10.81% | -10.81% | ok |
| indexeddb / range_scan_10000 | 16.99 | 23.68 | +39.38% | -28.26% | -34.85% | -34.85% | ok |
| moyodb / small_tx_1000_commits | 0.16 | 2.06 | +1196.70% | -92.29% | -92.29% | -92.29% | ok |
| indexeddb / small_tx_1000_commits | 9.39 | 8.47 | -9.82% | +10.89% | +10.89% | +10.89% | throughput -9.82%; avg latency +10.89%; p95 latency +10.89%; p99 latency +10.89% |
| moyodb / large_value_64kb | 0.04 | 0.10 | +144.25% | -59.06% | -60.10% | -60.10% | ok |
| indexeddb / large_value_64kb | 15.70 | 16.08 | +2.41% | -2.35% | -12.72% | -12.72% | ok |
| moyodb / large_value_1mb | 0.01 | 0.12 | +1303.87% | -92.88% | -92.34% | -92.34% | ok |
| indexeddb / large_value_1mb | 12.89 | 13.62 | +5.67% | -5.37% | +0.49% | +0.49% | ok |
| moyodb / recovery_after_dirty_close | 0.94 | 0.88 | -6.33% | +6.76% | +3.71% | +3.71% | throughput -6.33%; avg latency +6.76% |
| moyodb / snapshot_export_import | 3.47 | 4.11 | +18.41% | -15.55% | -15.18% | -15.18% | ok |
| moyodb / worker_roundtrip_overhead | 3.34 | 3.55 | +6.29% | -5.92% | -3.30% | -3.30% | ok |
