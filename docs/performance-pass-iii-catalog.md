# Minimum-Work Pass III: incremental native catalogs

Base: `5f5c44a83a6ede32b06830ea95044d6b8941f4ef` in `eluvane/MoyoDB-dev`.
This is a contribution-only implementation branch, not a standalone distribution.

## Removed work

A transaction previously cloned every store's catalog metadata at begin. A
one-store commit cloned the catalog again while planning, compared the complete
maps, walked and retired the entire old catalog tree, cloned the final map for
the full builder, and encoded every catalog record into replacement pages. That
work scaled with unrelated stores, including SDK-owned index stores.

Transactions now share immutable catalog maps. Ordinary commit plans carry only
store metadata deltas; the existing B-tree copy-on-write mutation path rewrites
the union of affected root-to-leaf paths. There is no full catalog equality
comparison, retirement traversal, or full builder in ordinary commit planning.
Only changed scalar metadata is encoded, with the legacy-floor exception below.

After durable WAL publication, a nonempty delta updates the map in place when
there is no reader of the current version. Otherwise `Arc::make_mut` preserves
the reader's map with one copy. The committing writer releases its own snapshot
before planning, so it does not accidentally force that copy. Older readers of
an already-detached version do not force another copy on every later commit.
Empty and scalar-only deltas do not detach the store map.

## Structural before / after

These are **code-derived work counts and bounds**, not measured benchmark
results. `N` is the number of catalog store entries and `K` the changed entries.
Counts exclude value trees and change-feed payloads, which this pass does not
optimize.

| Work | Before | After |
| --- | --- | --- |
| Catalog entries cloned at engine transaction begin | `N` | `0`; one shared reference |
| Full-map entry clones for begin + changed one-store commit, no readers | Approximately `3N` | `0` |
| Catalog-wide map comparison during planning | Up to `N` entries | None; delta emptiness plus scalar comparisons |
| Store metadata serialized for catalog mutation | All `N` records | Changed records only (`K`) |
| Catalog pages retired and emitted | Entire catalog tree | Affected COW paths and necessary rebalancing |
| Scalar-only commit with a live reader | Catalog clones and full rebuild | No store-map clone; affected metadata path only |
| First store-map mutation with a reader of the current version | Unconditional full clones | One `N`-entry COW copy; subsequent writes need not repeat it |

For example, updating one existing store in a running 8,192-store database with
the feed disabled, unchanged schema/policy and an effective floor of zero removes
24,576 full-map entry clones across begin/planning/build preparation. The
`encode_store_metadata` call count falls from 8,192 to one. This is not a claim
that the operation touches only one page: existing cells in touched nodes still
need decoding/copying, and the shared B-tree mutator may rebalance nodes.

The structural tests print actual catalog image/retirement counts and complete
user-commit WAL image/byte counts when executed. They cover 1, 128, 1,024 and
8,192 stores, with and without the change feed. Their page-count ceilings are
regression bounds for these fixtures, not a universal maximum for arbitrary
catalogs or batches.

## Semantics and compatibility

WAL append/flush, page staging, checkpoint barriers, engine poisoning, recovery,
free-page promotion and retirement ordering are unchanged. Catalog deltas are
not published to engine metadata before the original durable publication point.
Old snapshots keep both their metadata maps and their reachable page versions.
The same B-tree mutation code already used for user stores retires only replaced
catalog pages; untouched subtrees remain shared.

Catalog keys, StoreMetadata bytes, page format, WAL format, snapshot file format,
TTL logic and change-feed record encoding are unchanged. Returning the change
feed policy to its default deletes its optional metadata record, matching the
full builder. Existing decoders continue accepting absent legacy default records.

On open, databases with a zero stored floor and no change-log store can receive
a synthesized nonzero floor. The delta writer reasserts a nonzero floor when the
base catalog has no change log, even if the effective floor did not change.
Otherwise the first logged commit could lose that floor on reopen. This is a
conservative exception: it may rewrite an already-persisted floor when the feed
is disabled; it avoids adding another lifetime-sensitive state flag.

Pager checksum/id validation and B-tree node validation remain on accessed paths.
Open/recovery still decode the complete catalog. An ordinary write no longer
incidentally scrubs unrelated catalog pages while retiring the whole tree;
corruption in an untouched page is detected when that page is accessed, not by
an unrelated write. No touched-path validation is bypassed.

The JavaScript API is unchanged. The Rust `Snapshot.catalog` field changes from
`CatalogMap` to `Arc<CatalogMap>`; callers constructing the struct directly or
mutating that field must adapt. `Snapshot::new(..., &CatalogMap)` retains its
signature and owned-input behavior. Serde's `rc` feature preserves the serialized
map representation; sharing is an in-memory implementation detail.

## Verification and measurements

Added 12 unit tests covering shared begin, writer snapshot release, live readers
across clear/drop/create and checkpoint/reuse, empty/scalar commits, delta
cancellation, a mixed-mutation full-builder oracle, catalog/WAL work bounds,
legacy floor persistence, failed WAL publication/recovery, touched-root
corruption, serde shape and policy/retention changes.

The existing `commit` Criterion target now also contains `catalog_scaling`:
readonly begin/get/rollback and one-store commits with the feed disabled, enabled,
and with a live reader. Setup and engine teardown are outside timed write
operations; no whole-catalog `stats()` call is inside the new measured regions.
Copy the **same updated benchmark harness** to the baseline worktree before
comparing it with this branch. Use matching release settings, machines and sample
counts. These are MemoryBackend engine measurements, not browser/OPFS results.

**Execution status:** baseline file blob hashes were matched to GitHub, and the
patch was checked for whitespace errors and clean application. Rust tests,
Criterion timings, rustfmt, Clippy and WASM checks were **not executed**: this
environment has no Rust toolchain. The base commit's GitHub CI also reports failed
jobs without available steps/logs; that does not identify a code failure or prove
this implementation correct. Native structural counters and before/after latency
are therefore **not yet measured**. No browser, IndexedDB or speedup claim is made.

Reproduction with the repository's pinned toolchain:

```sh
cargo test -p moyodb-engine --lib engine::catalog_tests -- --nocapture
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p moyodb-engine --target wasm32-unknown-unknown
cargo bench -p moyodb-engine --bench commit -- catalog_scaling
```

## Remaining substantial work

A current-version live reader still forces a whole BTreeMap copy on its first
concurrent store-metadata mutation; this pass deliberately does not introduce a
persistent-map implementation. Snapshot import still uses the complete catalog
builder. The reusable-page vector is still cloned/sorted per commit, and oldest
snapshot discovery still scans active transactions. Large-value/change-feed
serialization, repeated point traversals and per-row indexed WASM calls remain
separate candidates; their relative cost needs fresh workload measurements.
