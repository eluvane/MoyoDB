import MoyoDbProofs.Main

/-!
Abstract key-value, WAL and transaction models, with a certificate for independently decoded
inline B-tree pages. Accepted trees agree with the expected map for every byte-string key.
Format-2 external descriptors receive structural length and extent-address validation.
External contents are rejected by the map certificate because PAY2 bodies are not supplied.
Finite conformance checks compare Rust page images, mutations, lookups, scans and retained
copy-on-write roots. They do not prove universal Rust refinement or cover checksums,
overflow or external payload bodies, OPFS, flush ordering or power loss.
-/
