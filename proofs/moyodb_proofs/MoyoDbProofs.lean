import MoyoDbProofs.Main

/-!
Abstract key-value, WAL and transaction models, with a certificate for independently decoded
inline B-tree pages. Accepted trees agree with the expected map for every byte-string key.
Finite conformance checks compare Rust page images, mutations, lookups, scans and retained
copy-on-write roots. They do not prove universal Rust refinement or cover checksums,
overflow, OPFS, flush ordering or power loss.
-/
