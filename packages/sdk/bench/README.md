# Browser benchmarks

This directory contains the browser benchmark suite for MoyoDB. It measures browser-runtime paths: TypeScript SDK, Worker transport, WebAssembly engine bindings, and OPFS persistence when a workload uses persistent storage.

Native Rust Criterion benches are useful for engine-core microbenchmarks, but they do **not** prove browser performance. Browser results depend on the browser implementation, storage backend, quota state, cache state, release/debug build mode, value size, batch size, transaction boundary, and whether the benchmark is using SDK calls, Worker-local engine calls, or raw OPFS.

## Commands

```bash
cd packages/sdk
npm run build:wasm:release
npm run bench:browser     # smoke profile, MoyoDB + IndexedDB, Chromium project
npm run bench:indexeddb   # smoke profile, IndexedDB only
npm run bench:opfs        # smoke profile, MoyoDB/OPFS only
npm run bench:report      # summarize raw JSON files from bench/results
```

`npm run build`, `npm run dev`, `npm run dev:test`, and `npm run test:e2e` all build the WASM package with the release script. `npm run build:wasm:dev` exists only for local debugging and must not be used for published benchmark numbers.

The Playwright benchmark test is skipped during normal `npm run test:e2e`. It runs when invoked by one of the benchmark npm scripts or when `MOYODB_RUN_BENCH=1` is set.

Optional environment variables:

\- `MOYODB_BENCH_PROFILE=smoke|standard|full` (`full` is the practical suite, not the million-row rows)\
\- `MOYODB_BENCH_ENGINE=all|moyodb|indexeddb`\
\- `MOYODB_BENCH_PERSISTENT_CONTEXT=1` uses a fresh, temporary persistent browser profile for the suite (default `0`: Playwright's incognito context)\
\- `MOYODB_BENCH_WORKLOADS=bulk_insert_1m_batched_10000,random_get_10k_from_1m_bulk`\
\- `MOYODB_BENCH_SAMPLE_COUNT=1`\
\- `MOYODB_BENCH_WARMUP_COUNT=0`\
\- `MOYODB_BENCH_WORKLOAD_TIMEOUT_MS=300000`\
\- `MOYODB_BENCH_IDB_DURABILITY=strict|relaxed|default` (default `strict`, matching MoyoDB's durable WAL flush per commit)\
\- `MOYODB_BENCH_GIT_SHA=<sha>` overrides the revision the launcher reads from `git` (or `GITHUB_SHA` in CI)\
\- `MOYODB_CHROMIUM_EXECUTABLE_PATH=/path/to/chromium` for local systems without Playwright-managed browsers\
\- `MOYODB_DISABLE_VIDEO=1` for environments that do not have Playwright's bundled ffmpeg

To compare the same workloads in both storage modes:

```bash
MOYODB_BENCH_PERSISTENT_CONTEXT=0 MOYODB_BENCH_PROFILE=full npm run bench:browser
MOYODB_BENCH_PERSISTENT_CONTEXT=1 MOYODB_BENCH_PROFILE=full npm run bench:browser
```

The persistent profile is created once per test, shared by both engines, and removed after its browser closes. It never uses your normal browser profile. Reports record the actual `persistentContext` setting; persistent results have a `-persistent` filename suffix so they do not overwrite the default results. The launcher retains the configured executable, headless setting, and context options.

Browser context mode can substantially change OPFS's storage implementation and call costs. Keep the mode fixed for before/after comparisons and report both modes separately; switching modes is not a MoyoDB optimization. Strict API settings also do not establish durable-device performance: record the host filesystem/device, especially for memory-backed filesystems or environments where flush/fsync has no durable device behind it.

## Manual browser run

```bash
cd packages/sdk
npm run dev
# open http://127.0.0.1:4173/bench/browser-bench.html
```

Use the page controls to run `smoke`, `standard`, or `full` profiles and export raw JSON.

For a regular persistent browser profile, set `window.moyodbBench.defaultBenchOptions.persistentContext = true` in the console before using the controls. The page cannot detect incognito mode; this setting labels the report and does not change browser storage mode.

## Timing rules

Every workload has an optional `prepare()` step, a measured `run()` step, and an untimed `verify()` step that runs before cleanup.

The measured region uses `performance.now()` inside the browser page and excludes:

\- deterministic key/value generation;\
\- random key generation;\
\- database delete/cleanup;\
\- database open/create unless the workload name explicitly says `open`;\
\- preload for read/scan workloads;\
\- report generation and JSON stringify;\
\- Playwright `page.evaluate()` per operation.

For million-row read/scan workloads, preload uses one setup transaction so the read benchmark is not blocked by the separate batched-write stress path. Compare those rows as read latency only; use the insert rows for write-path numbers.

Warmup samples are recorded separately and excluded from percentiles. Raw measured samples are preserved in `bench/results/*.json`.

`open_empty_db` opens a fresh database in a fresh Worker, while `cold_open_after_100k` reopens an existing database in a fresh Worker. Neither proves first-runtime or device-cache coldness: suite capability/build-profile probes, setup/cleanup Workers, warmups, and earlier samples can warm the JavaScript module graph, compiled WASM, and storage caches. Setting warmups to zero does not disable those probes. Report a separate first-runtime experiment if that is the startup cost being investigated. The normal launcher serves SDK modules through Vite's development server with release WASM; compare production bundle startup separately and retain the reported build modes.

## Diagnostic layer workloads

The smoke profile includes small diagnostics so regressions can be assigned to the right layer before optimizing code:

| Workload                         | Layer isolated                                                                        |
| -------------------------------- | ------------------------------------------------------------------------------------- |
| `noop_js_loop_1m`                | JavaScript loop overhead only.                                                        |
| `noop_worker_roundtrip_10k`      | Legacy raw Worker postMessage/request-response latency without SDK/WASM/OPFS.         |
| `worker_roundtrip_noop`          | Internal Worker protocol-style no-op roundtrip.                                       |
| `worker_roundtrip_small_payload` | Internal Worker protocol-style 32-byte payload roundtrip.                             |
| `worker_roundtrip_256b_payload`  | Internal Worker protocol-style 256-byte binary payload roundtrip.                     |
| `worker_roundtrip_64kb_payload`  | Internal Worker protocol-style 64 KiB structured-clone payload roundtrip.             |
| `worker_binary_transfer_64kb`    | Internal Worker protocol-style 64 KiB transferred payload roundtrip.                  |
| `noop_wasm_call_100k`            | Repeated JS-to-WASM method dispatch inside a Worker.                                  |
| `encode_decode_10k_256b`         | Benchmark key/value allocation and byte-copy cost.                                    |
| `opfs_raw_write_100mb`           | Raw OPFS SyncAccessHandle sequential write throughput.                                |
| `opfs_raw_read_random_10k`       | Raw OPFS SyncAccessHandle random read throughput.                                     |
| `sdk_put_1k_single_calls`        | Smoke-sized public SDK single-call write loop; intentionally shows per-call overhead. |
| `sdk_bulk_put_10k`               | Public SDK bulk write path with data generation/setup excluded.                       |
| `engine_stage_put_10k_rollback`  | Worker-local WASM transaction staging without commit/fsync.                           |
| `engine_bulk_put_10k`            | Worker-local WASM + engine commit path, bypassing public SDK payload transfer.        |
| `indexeddb_bulk_put_10k`         | IndexedDB single-transaction baseline with setup excluded.                            |

The diagnostic names are intentionally explicit. Do not publish one of them as an end-to-end product benchmark without explaining which layer it isolates.

## End-to-end workloads

Representative write/read/scan rows include:

\- `open_empty_db`\
\- `bulk_insert_10k` (automatic). `bulk_insert_100k`, `bulk_insert_1m`, `bulk_insert_1m_batched_1000`, `bulk_insert_1m_batched_10000`, `bulk_insert_1m_single_tx`, and `cold_insert_1m_single_tx` are opt-in (`manual`)\
\- `sdk_put_10k_single_calls` opt-in. Ten thousand separate commits ran for about an hour on one thread\
\- `point_get_random_10k` (sequential), `point_get_random_10k_pipelined`, and `point_get_random_10k_bulk` (MoyoDB only) are automatic. `point_get_random_100k` and `point_get_random_1m` are opt-in\
\- `point_get_random_1m_preloaded`, `random_get_10k_from_1m`, `random_get_10k_from_1m_pipelined`, `random_get_10k_from_1m_bulk`, `range_scan_1000_from_1m`, `batch_tx_100k_values_256b`, and `cold_open_after_100k` are opt-in\
\- `range_scan_100`, `range_scan_1000`, `range_scan_10000` run against a 10k-row database\
\- `reverse_scan_limit_1` reads the last row with a reverse scan bounded to one result\
\- `small_tx_1000_commits` is one sample\
\- `large_value_64kb`, `large_value_1mb`\
\- `recovery_after_dirty_close`\
\- `snapshot_export_import`\
\- `worker_roundtrip_overhead`

The `bulk_insert_1m_single_tx`/`cold_insert_1m_single_tx` row is a pathological large single-transaction probe for the current architecture. It must be reported next to batched rows, commit diagnostics, and IndexedDB transaction-boundary notes. It is not the headline browser benchmark by itself.

## Fair IndexedDB baseline

IndexedDB is the browser's standard transactional object store. MoyoDB explores a lower-level OPFS-backed storage-engine design for workloads where predictable batch performance and recovery behavior matter.

For comparable workloads, both engines use:

\- the same binary keys (IndexedDB orders binary keys bytewise, like MoyoDB) and the same deterministic values;\
\- the same random index sequence per sample, generated once in `workloads.ts`;\
\- the same record count, value size, batch size, warmup count, measured sample count, and transaction boundaries;\
\- explicit durability: IndexedDB readwrite transactions pass `{ durability }` (default `strict`); MoyoDB flushes WAL before a commit resolves and uses bounded checkpoints to install pages and flush the main file and manifest.

`MOYODB_BENCH_IDB_DURABILITY` controls explicit readwrite transactions. `indexedDB.open()` has no durability parameter for its implicit `versionchange` transaction. Startup/open rows therefore compare API latency; selecting `strict` does not establish strict persistence parity for that implicit transaction.

Random reads are split by request mode, because issuing requests one at a time and queueing them all measure different things:

| Mode       | MoyoDB                                 | IndexedDB                                           |
| ---------- | -------------------------------------- | --------------------------------------------------- |
| sequential | `await tx.get()` per key               | next `store.get()` issued from the previous success |
| pipelined  | all `tx.get()` calls, then `await` all | all `store.get()` requests queued at once           |
| bulk       | one `tx.getMany()`                     | not applicable: IndexedDB has no multi-key get      |

Range scans use each engine's bulk range API (`tx.scan` versus `getAllKeys` + `getAll`). The reverse scan uses `tx.scan({ reverse: true, limit: 1 })` versus a `prev` cursor stopped after one row.

After every timed sample, `verify()` checks the data against the deterministic dataset: read and scan results in full, and write samples by reading back up to 1,024 evenly spaced records. A mismatch fails the sample. Each result stores one content checksum per measured sample, and the report's content-parity table flags workloads where the engines disagree.

Data generation, opening the database, and preload stay outside the measured regions. The baseline does not intentionally slow IndexedDB down.

## WebKit/Safari handling

The MoyoDB OPFS path requires `FileSystemSyncAccessHandle` in a dedicated Worker. The suite probes that capability in a Worker and skips MoyoDB OPFS workloads only when the current browser/runtime does not expose it. A Playwright WebKit skip is therefore an environment/runtime result, not a claim that Safari as a browser never supports OPFS.

## Output fields

Each report records:

\- browser name/version, user agent, platform, timestamp, and webdriver/headless hints;\
\- the git revision (with a `-dirty` suffix for uncommitted tracked changes);\
\- SDK build mode, the WASM build profile reported at runtime by the engine's `buildProfile()`, backend path, persistent-context flag;\
\- IndexedDB and MoyoDB durability settings;\
\- per-sample content checksums;\
\- OPFS, Worker, BroadcastChannel, Web Locks, and SyncAccessHandle support flags;\
\- workload name, record count, key size, value size, batch size, transaction boundary;\
\- warmup samples, measured samples, p50/p95/p99/min/max/mean;\
\- notes, skip reasons, and errors.

## Paired engine-artifact comparison

`crossover-bench.mjs` compares two separately built release engine directories through the same SDK source, browser, context, and origin, with a fresh page for every artifact suite. Use Node 24+ and the repository's existing npm dependencies. For example, with the unchanged baseline in `../MoyoDB-before` and the patch in this checkout, run from this repository's root:

```bash
npm --prefix ../MoyoDB-before ci
npm --prefix ../MoyoDB-before/packages/sdk run build:wasm:release
npm ci
npm run build:wasm:release --workspace @moyodb/sdk
npx playwright install chromium
export BASELINE_ENGINE_DIR=../MoyoDB-before/packages/sdk/public/engine
export AFTER_ENGINE_DIR=./packages/sdk/public/engine
export BENCH_GIT_SHA=23ad23b4854b59c5c2edfd1ef9f5cf1f697ec694
export OUTPUT_DIR=./bench-results/crossover
MODE=persistent node packages/sdk/bench/crossover-bench.mjs
MODE=incognito node packages/sdk/bench/crossover-bench.mjs
node packages/sdk/bench/crossover-report.mjs
```

`BASELINE_ENGINE_DIR` is required; `AFTER_ENGINE_DIR` defaults to this checkout's generated engine. Both directories must remain unchanged during the run. `BENCH_SOURCE_DIR` optionally selects one shared source checkout; the default is this repository. `CHROMIUM_EXECUTABLE` optionally selects a browser executable. The crossover script defaults to `MODE=persistent`, creates its own temporary profile, and removes it after closing the browser. The ordinary Playwright launcher above retains its incognito default.

Each artifact receives six measured samples per workload and one initial warmup, in six balanced AB/BA rounds. The stock workloads, timed regions, strict explicit write transactions, and content verification are unchanged. The previous page closes after the suite completes its cleanup and before the selected artifact changes. The next fresh page runs the unchanged environment probe and keeps that artifact's main-page WASM instance alive only for its own suite. This gives both artifacts the same page and module lifetime.

Every served WASM, glue, and snippet file is checked against a SHA-256/byte-length manifest loaded before measurement. Exact request counts require one main-page probe plus one engine load per DB Worker; the cached main-page engine must still report a release build after the suite without fetching another asset. Both artifacts receive identical `no-store` headers. Empty-open is excluded because this fetch control changes startup cache behavior. Measure startup separately with the normal launcher.

An earlier one-page crossover retained only the first artifact in `detectWasmBuildProfile()`'s cached module import. Its Worker asset hashes were correct, but main-page WASM lifetime was asymmetric. Keep those measurements as separate diagnostics. The reporter requires fresh-page design version 2 and rejects the earlier reports; use a new output directory when changing measurement methods.

The report uses all six Moyo samples per artifact and one shared median of the twelve contemporary IndexedDB controls. It verifies distinct page instances, exact main-page/Worker asset counts, sample counts, artifact checks, and content parity, and retains raw samples and AB/BA results. To summarize only one completed mode, pass `persistent` or `incognito` to `crossover-report.mjs`; the default reads both. Keep the modes and any host storage limitations explicit when presenting the results.

## OPFS structural diagnostics

`opfs-diagnostic.mjs` uses the same opt-in Worker/WASM/OPFS hooks as the profiling pass. Method counts are not RPC counts; nested inclusive times cannot be added or used as stock/cold-start latency. Run with Node 24+ after separate release builds:

```bash
MEASUREMENT_KIND=structural-inclusive BENCH_GIT_SHA=23ad23b4854b59c5c2edfd1ef9f5cf1f697ec694 \
PERSISTENT=0 ENGINE_DIR=../MoyoDB-before/packages/sdk/public/engine OUTPUT=./bench-results/before-opfs.json \
SPECS='["large_value_64kb","large_value_1mb","small_tx_1000_commits"]' node packages/sdk/bench/opfs-diagnostic.mjs
```

Repeat with the changed `ENGINE_DIR` and a different `OUTPUT`; keep `REPO_DIR` (shared SDK source, default this checkout), browser, and `PERSISTENT=0|1` fixed. `CHROMIUM_EXECUTABLE` optionally selects Chromium. The report retains preparation and measured counters, byte counts, checksums, source/artifact fingerprints, and inclusive times. Engine assets are verified against their initial manifest before instrumentation; supplied directories must stay unchanged. Persistent runs create and remove their own profile under the output directory.

## Performance-gap pass (2026-10-03)

The base is `MoyoDB-dev` main commit `23feb4d67e9b698ab0a39deca1af8af7608097dc`. The compact [measurement evidence](performance-gap-2026-10-03.json) retains raw samples, warmups, source/WASM fingerprints, transaction shapes, content-parity results, and per-launch variation. The changed variant is that commit **plus this patch**, not another committed revision.

### Comparable browser results

These are complete public-SDK workloads in Linux Chromium 153.0.8010.0, using Vite-served SDK source and release WASM. Four fresh persistent browser profiles ran in before/after/after/before order. Each row had two warmups and five measured samples per launch: ten Moyo samples per variant and twenty IndexedDB controls in total. Both engines retained the stock inputs, timed regions, transaction boundaries, and strict durability. All selected samples passed content verification.

The IndexedDB column is one shared median of all twenty controls. Its values fluctuated between launches; pooling provides a common reference and does not remove host drift. The evidence preserves that spread. These are four browser launches, not twenty independent replications. Fresh empty DB open is **not** cold browser/module startup: the stock harness checks the WASM build profile before sampling.

| Workload                          | Moyo before, ms | Moyo after, ms | Shared IndexedDB, ms | Gap before | Gap after |
| --------------------------------- | --------------: | -------------: | -------------------: | ---------: | --------: |
| 1,000 values, 64 KiB; ten commits |          397.00 |         295.00 |                54.60 |      7.27x |     5.40x |
| 64 values, 1 MiB; eight commits   |          350.35 |         286.65 |                72.35 |      4.84x |     3.96x |
| 1,000 separate commits            |          173.70 |         158.90 |               202.70 |      0.86x |     0.78x |
| 10,000 pipelined reads            |          143.00 |         130.50 |               171.00 |      0.84x |     0.76x |
| Reverse scan, limit one           |            0.70 |           0.60 |                 0.45 |      1.56x |     1.33x |
| Fresh empty DB open               |           50.25 |          34.15 |                 0.50 |    100.50x |    68.30x |

Large-write medians improved in both A/B pairs. Their measured reductions are 25.7% and 18.2%. The other rows require more caution: pipelined-read direction reverses in the second pair, and the observed startup change exceeds the separately measured 2–3 ms capability probe. Do not attribute every movement in this table to the patch. The original external timings are not the control for this experiment; the current revision and this browser/storage environment already put separate commits and pipelined reads ahead of IndexedDB.

### Cause and preserved physical work

Separate, instrumented browser runs placed most large-write time inside WASM commit. Temporary Rust stage timers in an isolated diagnostic build measured the following averages over two samples after one warmup. These diagnostic timings are **not** the headline A/B samples, and WAL encoding includes both copying and checksum calculation.

| Diagnostic phase                        | 64 KiB before / after, ms | 1 MiB before / after, ms |
| --------------------------------------- | ------------------------: | -----------------------: |
| Commit planning                         |             120.5 / 124.8 |            110.9 / 112.0 |
| WAL encoding                            |              104.9 / 28.1 |              96.7 / 20.1 |
| WAL backend write                       |               15.8 / 20.8 |              14.5 / 15.4 |
| Checkpoint, including its storage calls |               60.7 / 58.4 |              47.3 / 48.1 |
| Complete WASM commit                    |             306.3 / 236.4 |            272.8 / 199.0 |

The page encoder already computes a full CRC with the page checksum field zeroed. The generated-page WAL path now combines that CRC with the 40-byte WAL prefix and the fixed correction for the embedded little-endian checksum. The resulting WAL bytes are identical to the generic encoder. The generic public append APIs still hash arbitrary input in full; read/recovery validation is unchanged. Two compile-time tables occupy 8 KiB and require no runtime initialization.

| Structural measurement per complete workload |     64 KiB before / after |      1 MiB before / after |
| -------------------------------------------- | ------------------------: | ------------------------: |
| Generated pages                              |           34,066 / 34,066 |           33,176 / 33,176 |
| Bytes hashed for WAL page records            |   140,896,976 / 1,362,640 |   137,215,936 / 1,327,040 |
| WAL writes                                   |                   10 / 10 |                     8 / 8 |
| Bytes written to WAL                         | 140,897,456 / 140,897,456 | 137,216,320 / 137,216,320 |
| Main-file writes                             |                 547 / 547 |                 533 / 533 |
| Bytes written to main file                   | 139,534,336 / 139,534,336 | 135,897,088 / 135,897,088 |
| WAL / main / manifest flush calls            |     15 / 5 / 5, unchanged |     16 / 8 / 8, unchanged |

Each generated page therefore loses one redundant 4,096-byte checksum scan; its complete page checksum remains. Exact work-accounting tests exercise the real commit callsite at 1, 8, and 100 values, and differential tests compare complete WAL bytes, validation order, partial writes, and corrupt-input errors. The startup change also removes six synthetic capability-probe storage calls from each open/delete while preserving actual database handle acquisition; it eliminates contention on the shared probe file.

The remaining large-write costs include constructing user and change-feed trees, their required page checksums/copies, and checkpoint work. The enabled change feed retains complete values in its own tree: this workload still writes roughly twice its payload into page images and then persists WAL plus main-file data. Sharing those large payloads would require a separate lifetime/reclamation design. Startup is dominated by Worker/module/WASM initialization in the diagnostic traces. Pipelined reads already use batched transport; the reverse-scan workload retains explicit begin/scan/rollback boundaries.

### Reproduction

Install the locked dependencies and the repository's Rust/WASM/browser tooling. Build release WASM in both the unchanged detached worktree and the patched checkout. From the patched repository root:

```bash
git worktree add --detach ../MoyoDB-before 23feb4d67e9b698ab0a39deca1af8af7608097dc
(cd ../MoyoDB-before && npm ci && npm run build:wasm:release --workspace @moyodb/sdk)
npm run build --workspace @moyodb/sdk

node packages/sdk/scripts/bench-performance-gap.mjs --source-root ../MoyoDB-before --label before-a --samples 5 --warmups 2
node packages/sdk/scripts/bench-performance-gap.mjs --label after-a --sha 23feb4d67e9b698ab0a39deca1af8af7608097dc+patch --samples 5 --warmups 2 --reopen-check
node packages/sdk/scripts/bench-performance-gap.mjs --label after-b --sha 23feb4d67e9b698ab0a39deca1af8af7608097dc+patch --samples 5 --warmups 2
node packages/sdk/scripts/bench-performance-gap.mjs --source-root ../MoyoDB-before --label before-b --samples 5 --warmups 2
```

The runner starts Vite and Chromium in the same process environment and saves raw JSON under ignored `bench/results/`. `MOYODB_CHROMIUM_EXECUTABLE_PATH` can select an installed Chromium. Do not run compilation or other benchmarks concurrently. The optional reopen probe is a separate 128 x 4 KiB clean-close/reopen check for both engines; it is not a power-loss or large-value crash test.

For symmetric scaling, use `--large-value-sizes 4096,16384,65536,262144,1048576 --samples 3 --warmups 1` on each source tree. This retains 64 MiB total payload and eight 8 MiB transactions at every size, using the stock runners and verification. Write verification reads up to 1,024 evenly spaced records, so it samples the smaller-value scaling rows; the two original large-write rows are verified in full.

The executed scaling series improved all five sizes. These medians come from a separate pair of launches with three measured samples per engine/size and one warmup; the IndexedDB reference pools six controls. Raw values and parity results are in the same evidence JSON.

| Value size | Values | Moyo before, ms | Moyo after, ms | Shared IndexedDB, ms | Moyo reduction |
| ---------- | -----: | --------------: | -------------: | -------------------: | -------------: |
| 4 KiB      | 16,384 |           883.5 |          595.8 |               975.85 |          32.6% |
| 16 KiB     |  4,096 |           502.1 |          408.8 |               522.35 |          18.6% |
| 64 KiB     |  1,024 |           400.9 |          334.9 |                71.60 |          16.5% |
| 256 KiB    |    256 |           401.1 |          304.7 |                42.30 |          24.0% |
| 1 MiB      |     64 |           359.2 |          299.4 |                83.35 |          16.6% |

For separate OPFS, Worker-message, startup, and WASM-call accounting, add `--diagnostic --engine-timings --engines moyodb`. This uses instrumentation and disables HTTP cache through routing. Its `diagnostics.records[].elapsedMs` excludes snapshot collection but still includes hooks; the stock `rawSamples` also include snapshot requests. Neither is an uninstrumented comparison. The shipped runner measures public call/storage boundaries; the temporary private Rust stage timers used above are not included in the runtime patch.

Verification performed for this pass: 200 native Rust tests (one pre-existing ignored test), native/WASM Clippy, 128 SDK work tests, 49 OPFS-shim tests, 83 Chromium correctness tests (benchmark and soak opt-ins skipped), one separate ten-second randomized model/reopen/snapshot test, typecheck, scoped formatting/lint, dependency checks, and the complete release SDK/WASM build. The 27 Criterion test-mode smoke cases were not timed native benchmarks. Firefox and WebKit were not run in this environment.

## Payload planning follow-up (2026-10-03)

This follow-up is based on `MoyoDB-dev` main `c90d16df28b17a97d13132a5dcef471a572f42b8`. Main advanced during the investigation, so the final comparison was rebuilt and rerun from that commit. Generated-WAL-CRC reuse, startup-probe removal, the OPFS append-offset cache and 4 MiB checkpoint batching are present in both final variants. This patch changes payload preparation and page planning. The [compact evidence](performance-gap-followup-2026-10-03.json) separates the final measurements from supporting historical experiments.

### Full browser comparison

Four fresh persistent Chromium 153.0.8010.0 profiles each held two immutable source origins. Visits alternated ABBA/BAAB/ABBA/BAAB; origin assignments also alternated. Each variant had one warmup per profile and eight measured samples overall. The IndexedDB column pools sixteen controls. Both variants use release WASM and unchanged stock public-SDK run/verify functions, with matching stock random seeds. There was no request interception or cache-policy change in this comparison.

| Workload                                | Moyo before, ms | Moyo after, ms | Shared IndexedDB, ms | Gap before | Gap after |
| --------------------------------------- | --------------: | -------------: | -------------------: | ---------: | --------: |
| 1,000 values, 64 KiB; ten commits       |          344.55 |         254.60 |                70.85 |      4.86x |     3.59x |
| 64 values, 1 MiB; eight commits         |          315.70 |         262.15 |                70.65 |      4.47x |     3.71x |
| 1,000 separate commits, 128-byte values |          190.15 |         181.20 |               216.70 |      0.88x |     0.84x |
| 10,000 pipelined reads                  |          148.20 |         149.20 |               165.30 |      0.90x |     0.90x |
| Reverse scan, limit one                 |            0.95 |           0.80 |                 0.60 |      1.58x |     1.33x |
| Fresh empty DB open                     |           39.90 |          42.25 |                 0.50 |     79.80x |    84.50x |

Both large-write workloads improve in all four profile medians, reducing pooled Moyo medians by 26.1% and 17.0%. The other four rows do not establish a substantial consistent improvement. Small commits improved in three of four profiles in this final comparison; a slowdown observed before rebasing is retained only as historical evidence. Observations within a profile share host/cache conditions, and the pooled IndexedDB median does not remove host drift. No universal speedup is claimed.

A separate two-sample instrumented small-commit check was slower after the change: mean engine-boundary time 99.70 → 117.10 ms and instrumented total 201.25 → 239.40 ms. The eight-sample balanced comparison above improved by 4.7%. These differing observations are preserved without claiming a substantial commit improvement.

Explicit IndexedDB readwrite transactions request `strict` durability, and Moyo retains its existing WAL flush and checkpoint publication semantics. `indexedDB.open()` has an implicit versionchange transaction with no API durability option, so the open row compares API latency. Fresh empty open creates a new DB and Worker after capability/module preflight and warmups; it is not cold browser/network/module startup.

Separate instrumented baseline opens spent 56.9–63.0 ms reaching Worker READY, then 21.8–22.4 ms loading/initializing the module, versus 6.1–8.4 ms in engine open. Instrumentation disables HTTP cache, so these phase timings are diagnostic only; engine timings include nested storage work. Pipelined reads retained 10,002 logical operations in 81 physical Worker messages each way, and reverse scan retained three serialized begin/scan/rollback round trips. Neither read workload made timed OPFS calls.

The external starting numbers are not this experiment's baseline: current main already includes several performance passes, and this host uses a persistent headless browser on a container filesystem. The earlier separate persistence qualification verified strict IDB transaction configuration and both engines' data after complete browser shutdown/relaunch; it was a clean restart check, not a hardware power-loss test. Hosted flush latency does not establish physical-device power-loss latency.

### Removed work and correctness boundary

Fresh diagnostics on the final base placed 296.30 → 233.60 ms inside engine calls for 64 KiB writes, while nested synchronous storage time was 54.20 → 58.20 ms. For 1 MiB writes the corresponding figures were 277.20 → 223.55 ms and 60.45 → 58.65 ms. These are means of two separately instrumented samples. Engine-boundary time includes generated JS bindings, WASM and storage; the nested times must not be added. Together with unchanged write/flush work and the checksum/copy accounting below, this supports computation and temporary payload materialization as the primary removed cost.

The historical investigation on `23ad23…` found approximately 80–95 ms of sampled CRC self time per large-write operation, mostly in the two overflow encodings. With shared payload preparation, sampled CRC self time was 42–49 ms: 38–42 ms in preparation and approximately 1–3 ms in overflow encoding. These are separate symbol-preserving profiling builds, with function-index mappings checked against identical noncustom sections. The new upstream commit did not change checksum/overflow/B-tree code, but these historical sampled timings are not subtracted from the final browser results.

Main already reused generated-page checksums for WAL records. It still independently materialized and hashed the same staged payload for the user tree and CHG1 change feed. Prepared values now borrow that immutable payload with separate small prefixes. One checksum pass visits the union of the two encodings' chunk boundaries; CRC composition produces the original checksum for each independent page, including its header and zero padding. The private API requires pointer-and-length identity, and no checksum cache survives the pair or commit. Generic arbitrary-byte encoders and stored-page/WAL read validation remain intact.

| Structural work per original workload       |     64 KiB before → after |      1 MiB before → after |
| ------------------------------------------- | ------------------------: | ------------------------: |
| Actual native checksum-input bytes          |  140,995,760 → 68,883,856 |  137,331,008 → 70,177,344 |
| Payload bytes copied within commit planning | 262,144,000 → 131,072,000 | 268,435,456 → 134,217,728 |
| Explicit overflow zero-fill bytes           |   139,264,000 → 6,580,000 |      135,790,592 → 44,800 |
| Generated overflow pages                    |           34,000 → 34,000 |           33,152 → 33,152 |
| Added chunk-checksum metadata               |         0 → 136,000 bytes |         0 → 132,608 bytes |
| Measured main-file write calls              |                   47 → 47 |                   53 → 53 |
| Measured main-file bytes                    | 139,534,336 → 139,534,336 | 135,897,088 → 135,897,088 |
| Measured WAL bytes                          | 140,897,456 → 140,897,456 | 137,216,320 → 137,216,320 |
| WAL/main/manifest flushes                   |           15/5/5 → 15/5/5 |           16/8/8 → 16/8/8 |

Checksum-input counts were freshly measured on actual `c90d16…` main and the candidate's native stock-shape fixtures, over begin/put-many/commit with default checkpoints. Clean-cache publication order can change verification of a few 4 KiB metadata pages. Copy and explicit zero-fill counts are source-derived requests, not measured DRAM traffic or peak memory; SDK transport, WAL assembly and checkpoint assembly are excluded. Four planning payload copies become the two final independent user/feed page copies, and the encoder zeroes only the unused tail after directly initializing metadata and used bytes.

Overflow images also leave the balanced-tree metadata map: tree nodes retain lookup/removal support, while final overflow images accumulate in a vector and returned images remain sorted by page ID. The isolated native allocation fixture grew from 12 to 95 metadata allocations across 8 to 512 chunks before this change, versus 13 to 13 afterward; required 4 KiB page buffers are excluded. Its regression gate also passes on the rebased candidate.

Checkpoint batching and append-offset caching are unchanged from the common base. WAL/main/manifest barriers, checkpoint thresholds, dirty pins and failure ordering retain their semantics. The remaining large-write work includes two independent persisted user/feed chains, contiguous WAL assembly and copying dirty runs into checkpoint buffers. Removing those gathering copies requires a different ownership/buffer design; sharing persisted payloads requires a separate lifetime and reclamation design. This patch retains the storage format and those durable writes.

### Scaling and reproduction

The fixed-byte series uses 64 MiB of input and eight 8 MiB transactions at every value size. Two ABBA/BAAB profiles provide four Moyo samples per variant and eight IndexedDB controls. Pooled medians improved at all five sizes. Both profile medians improved at four sizes; the 64 KiB row had one slower profile, including a 355.7 ms candidate sample, which is retained in the raw evidence. Stock write verification samples at most 1,024 records in the larger record-count rows; both original large-write rows verify all their values. The stock deterministic generator has repeating low-entropy patterns, so these results do not establish arbitrary-entropy payload performance.

| Value size | Values | Moyo before, ms | Moyo after, ms | Shared IndexedDB, ms |
| ---------- | -----: | --------------: | -------------: | -------------------: |
| 4 KiB      | 16,384 |          544.55 |         446.40 |               969.65 |
| 16 KiB     |  4,096 |          358.50 |         291.90 |               417.00 |
| 64 KiB     |  1,024 |          347.60 |         263.80 |                53.80 |
| 256 KiB    |    256 |          320.40 |         261.70 |                36.20 |
| 1 MiB      |     64 |          312.25 |         229.80 |                72.15 |

A historical fixed-record crossover check used 2,048 values at 1008, 1024, 2048, 4033, 4050, 4066, 8192 and 16384 bytes, in batches of 256. Its raw samples, including outliers and near-neutral 1008/4050-byte rows, remain in the evidence with the old commit and artifact scope. That experiment informed the absence of a special pairing threshold; it is not a fresh comparison against `c90d16…`.

Use independent Cargo target directories for separate worktrees; sharing a target can reuse a stale executable from another checkout. Install locked dependencies and Rust/WASM/Chromium tooling, then run from the patched repository root:

```bash
git worktree add --detach ../MoyoDB-before c90d16df28b17a97d13132a5dcef471a572f42b8
(cd ../MoyoDB-before && npm ci && CARGO_TARGET_DIR="$PWD/target" npm run build:wasm:release --workspace @moyodb/sdk)
npm ci
CARGO_TARGET_DIR="$PWD/target" npm run build --workspace @moyodb/sdk

node packages/sdk/scripts/bench-performance-abba.mjs --before ../MoyoDB-before --after . --profiles 4 --samples 1 --warmups 1 --out packages/sdk/bench/results/followup-full.json
node packages/sdk/scripts/bench-performance-abba.mjs --before ../MoyoDB-before --after . --profiles 2 --samples 1 --warmups 1 --value-sizes 4096,16384,65536,262144,1048576 --total-bytes 67108864 --batch-bytes 8388608 --out packages/sdk/bench/results/followup-scaling.json
node packages/sdk/scripts/bench-performance-abba.mjs --before ../MoyoDB-before --after . --profiles 1 --samples 2 --warmups 2 --value-sizes 1008,1024,2048,4033,4050,4066,8192,16384 --records 2048 --batch-size 256 --out packages/sdk/bench/results/followup-crossover.json

cargo test --locked --workspace
cargo test --locked -p moyodb-engine --lib stock_large_write_hash_work_accounting -- --ignored --nocapture
CLIPPY_CONF_DIR=.config/moyo/build cargo clippy --locked --workspace --all-targets -- -D warnings
CLIPPY_CONF_DIR=.config/moyo/build cargo clippy --locked --workspace --lib --target wasm32-unknown-unknown -- -D warnings
```

The runner verifies immutable sources, served JS/WASM/shim bytes, release mode, strict IDB configuration, matching transaction dimensions and content checksums within and across variants. Its full raw output records actual browser command lines. Keep compilation, unrelated benchmarks and tracked-file edits out of measurement windows; the runner checks both source fingerprints and tracked diff statistics.

Regression tests compare complete pages against the previous encoder and complete WAL/checkpoint bytes against generic checksumming, including raw-to-TTL rewrites, compression flags, long keys, clear/drop and feed retention. Tests also cover crash publication boundaries, corrupted headers/payloads/padding, old snapshots with independent feed history and root collapse/reused IDs. Native work-accounting gates protect the removed checksum and metadata work.

Fresh verification after rebasing: 217 native Rust tests; the separately invoked ignored stock checksum fixture; native/all-target and WASM Clippy with canonical repository configuration; rustfmt; 128 SDK work tests; 61 OPFS-shim tests; 83 Chromium correctness tests; TypeScript typecheck; and release SDK/WASM builds for both revisions. The normal native run ignored the stock fixture and one pre-existing test; the normal browser run skipped benchmark and soak opt-ins. A separate fresh full automatic 31-workload guard ran both revisions with one warmup and one sample per supported row: each passed 30 Moyo and 12 IndexedDB rows, with 20 expected unsupported rows and no errors. Its single-sample timings are not the headline comparison. Firefox, WebKit and a new soak run were not executed.

## Future baseline: SQLite WASM + OPFS

SQLite WASM + OPFS is a serious future comparison target because the SQLite project publishes WebAssembly/JavaScript documentation and documents persistent browser storage options via OPFS. This suite does not claim SQLite comparison results until a reproducible workload and raw JSON output are added.
