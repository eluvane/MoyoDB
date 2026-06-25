# MoyoDB quality configuration

This directory is the canonical home for repository quality tooling. Root files should stay limited to project manifests that tools genuinely need at the workspace root (`package.json`, `package-lock.json`, `Cargo.toml`, `Cargo.lock`, `.gitignore`, `.editorconfig`, and documentation). Old standalone quality configs should not be reintroduced in the repository root.

## Layout

```text
.config/moyo/
  build/          Rust build-time lint knobs such as Clippy configuration
  ci/             GitHub Actions helper config: actionlint, CodeQL, dependency review
  formatters/     Biome, Prettier, Taplo, rustfmt, and ignore policy
  lints/          ESLint, dependency graph, Markdown/YAML/package/name/unused-code lints
  quality/        repository-policy, JSON/docs, package-manager, workflow and Lean hygiene scripts
  security/       cargo-deny, gitleaks, OSV Scanner and Semgrep policy
  test/           test-runner notes and future colocated test configs
  typescript/     TypeScript project used by type-aware linting
```

## Canonical tool map

| Area                   | Local command                                              | Canonical config                                                                                                   |
| ---------------------- | ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| Prettier               | `npm run format:prettier`, `npm run format:check:prettier` | `.config/moyo/formatters/prettier.json`, `.config/moyo/formatters/prettierignore`                                  |
| Biome                  | `npm run format:check:biome`, `npm run lint:biome`         | `.config/moyo/formatters/biome.jsonc`                                                                              |
| Taplo/TOML             | `npm run format:toml`, `npm run lint:toml`                 | `.config/moyo/formatters/taplo.toml`                                                                               |
| rustfmt                | `npm run format:rust`, `npm run format:check:rust`         | `.config/moyo/formatters/rustfmt.toml`                                                                             |
| ESLint                 | `npm run lint:eslint`                                      | `.config/moyo/lints/eslint.config.mjs`, `.config/moyo/typescript/tsconfig.eslint.json`                             |
| Oxlint                 | `npm run lint:oxlint`                                      | `.config/moyo/lints/oxlint.json`                                                                                   |
| dependency-cruiser     | `npm run lint:dependency-cruiser`                          | `.config/moyo/lints/dependency-cruiser.cjs`                                                                        |
| Markdown               | `npm run lint:markdown`                                    | `.config/moyo/lints/markdownlint-cli2.jsonc`                                                                       |
| YAML                   | `npm run lint:yaml`                                        | `.config/moyo/lints/yamllint.yml`                                                                                  |
| GitHub Actions         | `npm run lint:actions`, `npm run lint:workflows`           | `.config/moyo/ci/actionlint.yml`, `.config/moyo/ci/run-actionlint.mjs`, `.config/moyo/quality/check-workflows.mjs` |
| package.json hygiene   | `npm run lint:package-json`                                | `.config/moyo/lints/npm-package-json-lint.json`                                                                    |
| naming conventions     | `npm run lint:names`                                       | `.config/moyo/lints/ls-lint.yml`                                                                                   |
| JSON validation        | `npm run lint:json`                                        | `.config/moyo/quality/check-json-files.mjs`                                                                        |
| Knip                   | `npm run quality:knip`                                     | `.config/moyo/lints/knip.json`                                                                                     |
| syncpack               | `npm run quality:syncpack`                                 | `.config/moyo/lints/syncpack.config.cjs`                                                                           |
| package-manager policy | `npm run quality:package-manager`                          | `.config/moyo/quality/check-package-manager.mjs`                                                                   |
| docs command drift     | `npm run quality:docs`                                     | `.config/moyo/quality/check-docs-links.mjs`                                                                        |
| Clippy                 | `npm run lint:rust`                                        | `.config/moyo/build/clippy.toml` via `CLIPPY_CONF_DIR`                                                             |
| cargo-deny             | `npm run security:cargo-deny`                              | `.config/moyo/security/cargo-deny.toml`                                                                            |
| gitleaks               | `npm run security:gitleaks`                                | `.config/moyo/security/gitleaks.toml`                                                                              |
| OSV Scanner            | `npm run security:osv`                                     | `.config/moyo/security/osv-scanner.toml`                                                                           |
| Semgrep                | `npm run security:semgrep`                                 | `.config/moyo/security/semgrep.yml`                                                                                |
| CodeQL                 | `.github/workflows/codeql.yml`                             | `.config/moyo/ci/codeql-config.yml`                                                                                |
| Dependency Review      | `.github/workflows/ci.yml` security job                    | `.config/moyo/ci/dependency-review-config.yml`                                                                     |
| Lean hygiene           | `npm run lint:lean`                                        | `.config/moyo/quality/check-lean-hygiene.mjs` plus Lake/elan project files                                         |

## Local commands

Install the Node workspace from the repository root:

```bash
npm ci
```

The main developer commands are:

```bash
npm run format
npm run format:check
npm run lint
npm run lint:typescript
npm run lint:rust
npm run lint:lean
npm run lint:repo
npm run quality
npm run security
npm run test
npm run build
npm run bench:collect:smoke
npm run check
npm run check:all
```

Some commands intentionally call ecosystem tools that are not bundled as Node dependencies:

- `npm run lint:rust` needs Rust, Clippy, and `cargo-machete`.
- `npm run lint:lean` needs elan/Lake and uses the pinned `proofs/moyodb_proofs/lean-toolchain`.
- `npm run lint:yaml` needs Python `yamllint`.
- `npm run security:cargo-deny` needs `cargo-deny`.
- `npm run security:gitleaks` needs `gitleaks`.
- `npm run security:osv` needs `osv-scanner`.
- `npm run security:semgrep` needs `semgrep`.
- `npm run test`, `npm run build`, and browser benchmarks need Rust, the `wasm32-unknown-unknown` target, `wasm-pack`, and Playwright browser dependencies.

CI installs those tools explicitly instead of relying on global machine state.

## CI jobs

The primary workflow is `.github/workflows/ci.yml` and uses these blocking jobs:

- `format`
- `lint:typescript`
- `lint:rust`
- `lint:lean`
- `lint:repo`
- `quality`
- `security`
- `test`
- `build`
- `benchmark compile`

Code scanning is split into `.github/workflows/codeql.yml` so the CodeQL setup can use `.config/moyo/ci/codeql-config.yml` and the tighter `security-events: write` permission only where needed. Benchmark execution and artifact comparison live in `.github/workflows/benchmarks.yml`; slow paranoid scans live in `.github/workflows/paranoid-quality.yml`.

## Blocking and non-blocking policy

Required CI jobs are blocking by default. There are no `continue-on-error: true` quality jobs. Long-running benchmark and paranoid scans are scheduled or manually dispatched, but they are still real gates when invoked.

A small set of legacy TypeScript and Oxlint findings is intentionally warning-level instead of error-level. These warnings are visible locally and in CI, but they do not hide parse errors, TypeScript failures, unresolved imports, dependency cycles, unsafe dependency classes, Rust warnings, Lean proof hygiene failures, package-manager drift, or security gate failures.

## Root shims and root files

No root quality-config shims are currently required.

The root `package.json` and `package-lock.json` are not shims; they are the canonical npm workspace manifest/lockfile so all Node-based quality tooling has one install graph. `Cargo.toml` and `Cargo.lock` remain at root because Cargo workspace manifests and lockfiles are project inputs, not lint configuration. Lean keeps its `lean-toolchain`, `lakefile.lean`, and `lake-manifest.json` inside `proofs/moyodb_proofs` because elan/Lake discover those files from the Lean package root.

## Lean policy

Lean checks follow the project-local best-practices baseline supplied with this maintenance pass:

- exact Lean toolchain, no floating `stable` or `nightly` alias;
- source scan for `sorry`, `admit`, `axiom`, `unsafe`, `partial`, disabled diagnostics, and suspicious Lake/build-time shell usage;
- `lake build --wfail`;
- `lake lint --builtin-lint`;
- `lake env leanchecker --fresh MoyoDbProofs`;
- committed exported proof artifacts checked for drift.
