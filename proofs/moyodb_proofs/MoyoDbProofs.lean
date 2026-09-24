import MoyoDbProofs.Main

/-!
Executable specification of MoyoDB's abstract semantics: a sorted association-list store,
a WAL of page-image and commit records, and write-batch transactions. The theorems hold for
this model only. They do not cover the Rust B-tree, the on-disk page and checksum format,
OPFS I/O, flush ordering, or power loss; the exported traces are replayed against the engine
as conformance scenarios.
-/
