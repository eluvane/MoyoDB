# MoyoDB benchmark governance

This directory is the canonical benchmark control plane for MoyoDB. It contains the tracked policy and suite metadata; generated benchmark output is written under `.benchmark/current`, `.benchmark/smoke`, `.benchmark/previous`, and `.benchmark/comparison` and is ignored by Git.

## What belongs here

- `suites.json` lists the supported benchmark suites and the local command names.
- `policy.json` defines the default regression thresholds used by `npm run bench:compare`.
- Generated `baseline.json`, `summary.md`, raw browser JSON and comparison reports belong in ignored subdirectories.

## Local commands

```bash
npm run bench:rust:compile
npm run bench:collect:smoke
npm run bench:compare
```

Use `npm run bench:rust` only when the machine is intended to execute native Criterion timings. Browser rows are collected from `packages/sdk/bench/results` and normalized by `scripts/benchmark/collect.mjs`.

## Reading results

MoyoDB has two intentionally separate benchmark layers: native Rust engine microbenchmarks and browser SDK/WASM/Worker/OPFS measurements. They are both useful, but they are not interchangeable. Compare only rows with the same workload shape, browser profile, sample count and persistence mode.
