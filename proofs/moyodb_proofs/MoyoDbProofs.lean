import MoyoDbProofs.Main

/-!
Abstract KV/WAL/transaction semantics and a proof-carrying checker of independently decoded
inline B-tree pages. Accepted trees refine their expected map for every byte-string key.
Runtime conformance checks actual Rust page images, mutations, lookup/scan and retained COW
roots. These are checker proofs and finite implementation checks, without a universal Rust
refinement proof or coverage of checksums, overflow, OPFS, flush ordering or power loss.
-/
