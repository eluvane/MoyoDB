# Strict Lean linting

Run `npm run lint:lean` from the repository root, or `lake lint --lint-all` from
`proofs/moyodb_proofs`. The Lean CI job runs the same Lake driver and the fresh
kernel checker. `MOYO_LAKE` can select the installed Lake executable for the
external driver on Windows.

The package remains on Lean 4.31.0. Its Lake dependencies and manifest pin full
Git commits:

| Tool       | Source pin                                           | Coverage                                                                     |
| ---------- | ---------------------------------------------------- | ---------------------------------------------------------------------------- |
| Heron      | `1b8526c644a49572a0a63f1176fc5ac2e1c96eea`           | All 22 registered checks, including informational suggestions                |
| JunkLinter | `bf2e7937c9a327f7d7b075c18830423c332b4d4b` (v4.31)   | Elaborated signatures and definition bodies, guarded and presence-only rules |
| Batteries  | `fa08db58b30eb033edcdab331bba000827f9f785` (v4.31.0) | All 15 environment linters, including the three disabled by default upstream |
| Lean       | pinned toolchain                                     | All built-in and extra linters, with warnings as errors                      |

Heron's four manual refactoring actions are editor actions, not diagnostics.
Its CLI's `--all` would include these reversible actions, such as flipping an
`if`, so the gate enables every check without requesting manual refactors.
All check findings, even `information`, fail the gate. Heron's zero exit status
alone is insufficient: the driver validates the JSON report and rejects findings
and import failures. Each source gets a fresh process to avoid the upstream
multi-file initializer reset issue.

JunkLinter runs with guard discharging, presence-only checks and definition-body
checks enabled. It does not scan theorem proof terms or ordinary instance bodies;
this is an upstream limitation, not a project suppression. Its production package
requires no mathlib. Batteries runs its full default set plus `docBlameThm`,
`checkType` and `explicitVarsOfIff`.

The driver enumerates every project Lean file, builds all modules and runs the
built-in linters on all of them. It audits executable roots and the legacy artifact
script as well as the proof library. The Lake file is also scanned by Heron and
validated by the source hygiene policy. Linters run outside the proof import graph
so their implementation dependencies do not enlarge the kernel-checking closure.

The policy forbids linter disabling, Batteries `nolint`, warning suppression and
Heron's internal analysis bypass. Fix findings in the source. The gate includes
small regression witnesses: an informational Heron finding must fail, guarded
subtraction must pass while unguarded subtraction fails, and a commutative `simp`
lemma must fail Batteries' `simpComm` check. Logs and witnesses stay in `.tmp`.

The native lean-fmt release history starts with Lean 4.32.0, and there is no release
tag for our 4.31.0 toolchain. It is not included in this gate; changing the proof
toolchain just to install a formatter would be a separate migration.

Upstream documentation: [Heron](https://github.com/wvhulle/heron),
[JunkLinter](https://github.com/junk-values/junk-value-linter),
[Batteries](https://github.com/leanprover-community/batteries),
[lean-fmt compatibility](https://github.com/jcreinhold/lean-fmt).
