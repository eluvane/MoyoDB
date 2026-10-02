# Lean B-tree page checker

Run `npm run test:btree-proof` from the repository root. It builds the pinned Lean checker,
exports a bounded corpus through real Rust B-tree operations, checks it and exercises negative
witnesses. The mandatory `lint-lean` CI job runs this gate.

On Windows, `MOYO_LAKE` can name an installed pinned `lake.exe` directly. The command uses four
Cargo jobs by default, respecting an existing `CARGO_BUILD_JOBS`. No benchmark runs.

## Connection to the implementation

[The Rust exporter](../crates/engine/tests/lean_btree_pages.rs) calls production
`apply_mutations`, `lookup` and `scan`. It reads actual backend bytes using a fresh pager after
each step, including unchanged pages. It exports inputs and observations, **not expected maps**.

[The independent Lean decoder](../proofs/moyodb_proofs/MoyoDbProofs/Pages/Decode.lean) reads
4096-byte pages with little-endian fields, header and slot bounds, inline/internal cells,
disjoint cell ranges and page IDs. Reachability rejects missing pages, cycles and sharing
inside one tree. Sharing between COW roots is allowed. Sibling links are not required to describe
the current tree: production cursors use a parent stack.

[The CLI](../proofs/moyodb_proofs/CheckBTreePages.lean) starts with an empty map and computes each
state with Lean insert/erase from the submitted mutations. It compares structure, complete
contents, lookup and bounded forward/reverse scans. Every retained root is interpreted using
the **subsequent actual page map**, with its independently computed earlier state.

The full corpus has three mandatory scenarios and 15 mandatory frames:

- Long byte keys force leaf and internal splits, a height-two tree, changed subtree minima,
  merges, root collapse and an empty root.
- Prefix/NUL/high-byte keys exercise ordering, overwrite, delete and empty values.
- Empty-root and no-op transitions include root ID zero and an encoded empty root leaf.

Missing scenarios, frame IDs, lookup probes, scan modes or retained roots fail explicitly.

## Kernel-checked properties

[Core.lean](../proofs/moyodb_proofs/MoyoDbProofs/Pages/Core.lean) defines physical decoded trees,
flattening, minimum separators, level/order/uniqueness invariants and the routing algorithm.
Acceptance constructs a `CertifiedTree` carrying proofs of structure, routing and expected
contents.

The routing certificate checks each present key. A separate theorem proves that routing can
never invent an entry. Together these establish equality with the flat-map lookup for **every
byte-string key**, including absent keys. This first version uses that extra finite routing
check; it does not derive the routing theorem from ordering invariants alone.

The raw checker soundness theorem also establishes that its certified tree is the interpretation
of the exact supplied raw pages and root. Checked byte slices have proved bounds and lengths.
Batch concatenation preserves the computed transition state. The library is built with warning
failure, Lake lint and `leanchecker --fresh`; no proof placeholders or additional axioms are used.

These statements concern Lean's definitions. They are not a universal refinement proof of the
Rust implementation or a complete formal proof of every wire-format decoding rule.

## Negative witnesses

[The gate](../.config/moyo/quality/check-btree-pages.mjs) requires 12 malformed/incomplete cases
to fail with the intended diagnostic: missing/duplicate pages, overlapping cells, excessive
cell lengths, cycles, overflow values, corruption of a retained root and omitted coverage.

It then copies only the workspace manifests and engine crate into an ignored temporary directory
and applies two actual production changes in those copies:

1. Leaf packing emits the last key as separator instead of the minimum. Lean must report the
   affected page and separator/minimum mismatch.
2. Lookup routing uses `<` instead of `<=`. Lean must report a lookup mismatch at a separator.

Both mutants must compile and export their corpus successfully; compiler/exporter failure is not
accepted as a killed mutant. The exporter also asserts that their complete scan still matches the
insertion inputs. Expected states and the Lean checker remain unchanged.

The user's source tree and Git are never patched by mutation checks. Generated corpora, logs
and isolated copies remain under `.tmp/lean-btree-checker`.
Mutants have distinct package and library names, isolating their Cargo artifacts while reusing
dependency builds. The original package must pass again after both mutants.

## Scope and cost

The checker covers one inline B-tree at a time, including independently validated retained roots.
Reachable overflow pages/values are explicitly rejected. CRC/checksum correctness, allocation
liveness, catalog-root decoding, WAL/OPFS durability and power loss are outside this pass.

The trusted boundary includes agreement of the byte schema with the actual format, capture
completeness, serialization, the Lean compiler/runtime and the Rust toolchain. The proof is about
the checked interpretation; concrete Rust executions are tested through conformance.

Input is bounded to 32 MiB, 256 supplied pages per frame, 128 mutations, 256 lookup probes and
16 scans. Depth is bounded by the format's 48 internal levels. Pairwise key-order checks and the
routing certificate intentionally cost more than production lookup; they run only in verification.
There is no production hot-path overhead and no claimed database speedup.

The gate stops on unsupported reachable data, incomplete capture or a surviving meaningful
production mutant. Future extensions can add overflow and allocator/MVCC obligations without
silently treating this pass as covering them.
