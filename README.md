<p align="center">
  <img
    width="100%"
    src="https://capsule-render.vercel.app/api?type=waving&amp;height=220&amp;color=0:0B1220,50:1E1B4B,100:4F46E5&amp;text=MoyoDB&amp;fontColor=E2E8F0&amp;fontSize=54&amp;fontAlignY=50"
    alt="Header banner"
  />
</p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-1E293B?style=for-the-badge" />
  <img alt="WebAssembly" src="https://img.shields.io/badge/WebAssembly-1E293B?style=for-the-badge" />
  <img alt="TypeScript" src="https://img.shields.io/badge/TypeScript-1E293B?style=for-the-badge" />
  <img alt="JavaScript" src="https://img.shields.io/badge/JavaScript-1E293B?style=for-the-badge" />
  <img alt="Lean 4" src="https://img.shields.io/badge/Lean%204-1E293B?style=for-the-badge" />
  <img alt="npm" src="https://img.shields.io/badge/npm-1E293B?style=for-the-badge" />
</p>

Experimental Rust/WASM transactional key-value storage engine for browser apps.

It provides ordered byte keys, range scans, snapshots, TTL, recovery checks, and a TypeScript SDK that talks to a dedicated Worker-backed runtime over OPFS.

> Release: **1.0.0**. MoyoDB is experimental. Validate durability and compatibility for your workload before using it for production data.

## Quick start

```bash
npm install @moyodb/sdk@1.0.0
```

```ts
import { openDB, utf8Decode, utf8Encode } from '@moyodb/sdk';

const db = await openDB('app', { requestPersistence: false });
await db.createStore('kv');
await db.put('kv', utf8Encode('hello'), utf8Encode('world'));

const value = await db.get('kv', utf8Encode('hello'));
console.log(value ? utf8Decode(value) : null);

await db.close();
```

## Verification

- `proofs/moyodb_proofs` is a Lean 4 executable specification, not a proof of the storage engine. It models the store as a sorted association list, the WAL as a list of page-image and commit records, and transactions as write batches over that list. Its theorems (lookup after insert, idempotent delete, scan bounds, an incomplete WAL tail is ignored, snapshot stability) hold for that model only; they say nothing about the Rust B-tree, page and checksum layout, OPFS I/O, flush ordering, or power loss.
- The traces exported to `proofs/artifacts` are replayed against the Rust engine by `crates/engine/tests/proof_artifacts.rs` as conformance scenarios.
- Crash behavior is covered by tests: `crates/engine/tests/crash_matrix.rs`, `crates/engine/tests/fault_injection.rs` (failed and short writes plus torn flushes at every WAL, main-file, and superblock operation during commit, checkpoint, and recovery), and `crates/engine/js/opfs_shim.test.mjs` for control-file publication.

## Limitations

- Experimental software; storage format and SDK API may still change.
- Browser-only runtime; there is no Node.js persistence API.
- Requires OPFS and Worker-side sync access handles.
- One active owner per database and one write transaction at a time.
- Raw byte keys and values; no SQL/query planner.
- No cloud sync, multi-tab concurrent writers, SharedWorker runtime, or IndexedDB fallback.

## License

Copyright © 2026 Eluvane. The licensor is Eluvane; licensing requests go to [keiko1337@proton.me](mailto:keiko1337@proton.me). The official repository is [eluvane/MoyoDB](https://github.com/eluvane/MoyoDB).

MoyoDB **1.0.0** is the first release covered by the [Mother of Licenses 1.0](LICENSE) (`LicenseRef-MoL-1.0`). Earlier published releases retain their original licenses. Third-party dependencies and any separately licensed material retain their own licenses and notices.

MoL permits royalty-free use, including commercial use of unmodified code or Authorized Builds inside your own products. Private modifications and production patches are permitted for your own internal use, provided the modified code is not distributed, published, made available to third parties, or used to create a standalone or competing product or provide Project-as-a-Service. The upstream contribution workflow in Section 6 remains permitted. Other publication or distribution of modified versions, independent forks, resale of MoyoDB itself, and hosting MoyoDB itself as a service require written permission. MoyoDB under MoL is source-available, not OSI-approved open source.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) and the license before submitting changes. Contributions must include explicit acceptance of the contribution and patent terms in Sections 9 and 10.
