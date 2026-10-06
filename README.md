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

Rust/WASM transactional database for browsers, Node.js, and native Rust applications.

It provides ordered keys, typed records, SQL queries, indexes, snapshots, TTL, and recovery checks. Browser storage uses OPFS; Node.js and native Rust storage use local files. The SDK runs storage operations in a worker.

The SDK preserves the Release 1.0.1 API and stored encodings within the 1.x series. Incompatible storage changes require a distinct format version and an explicit migration path. Golden fixtures verify storage format 1 and snapshot versions 1–3; unknown formats fail before modifying durable data.

## Quick start

```bash
npm install @moyodb/sdk@1.2.0
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

## Node.js and native storage

```ts
import { openNodeDB, utf8Encode, utf8Decode } from '@moyodb/sdk/node';

const db = await openNodeDB('app', { directory: './data' });
if (!(await db.listStores()).includes('kv')) await db.createStore('kv');
await db.put('kv', utf8Encode('hello'), utf8Encode('world'));
console.log(utf8Decode((await db.get('kv', utf8Encode('hello')))!));
await db.close();
```

Node.js and `NativeFileBackend::open_db(root, name, create_if_missing)` share `root/stackdb/hex(UTF8 name)`. Database names contain valid Unicode and at most 127 UTF-8 bytes. File flushes use `fsync`; the directory lease excludes competing Node and native owners and releases an abandoned lease only after confirming that its process exited.

## Multiple clients and transactions

```ts
const db = await openDB('app', { workerMode: 'shared' });
```

Pages from the same origin share a storage owner through SharedWorker. Each client owns its transactions. Closing one handle preserves the other clients. When the page hosting the storage worker closes, another page reopens storage; database handles remain usable and active transactions receive invalidation.

Read and write transactions can overlap. Readers retain their snapshots. Commits publish sequentially; a writer whose snapshot is stale closes with `TransactionConflictError` before writing its changes. Start a new transaction to retry. Schema upgrades have one migration owner.

## Typed records

```ts
import { createRecordStore, jsonRecordCodec, keyCodecs } from '@moyodb/sdk';

const users = await createRecordStore(db, 'users', {
  key: keyCodecs.string,
  value: jsonRecordCodec<{ name: string }>()
});
await users.put('alice', { name: 'Alice' });
console.log(await users.get('alice'));
```

Record stores support CRUD, batches, ordered ranges, managed indexes, and automatic transactions. Scalar and tuple key codecs retain the existing byte encodings. JSON codecs reject values that JSON cannot preserve; pass a type guard to validate an application schema.

## SQL

```ts
import { createSqlClient } from '@moyodb/sdk';

const sql = createSqlClient(db);
await sql.execute('CREATE TABLE IF NOT EXISTS people (id INTEGER PRIMARY KEY, name TEXT NOT NULL)');
await sql.execute('INSERT INTO people VALUES (?, ?)', [1, 'Alice']);
console.log(await sql.query('SELECT name FROM people WHERE id = ?', [1]));
console.log(await sql.explain('SELECT name FROM people WHERE id = ?', [1]));
```

Each statement runs in one transaction. Supported commands are `CREATE TABLE`, `DROP TABLE`, `INSERT`, `SELECT`, `UPDATE`, and `DELETE`, with positional parameters, predicates, ordering, and pagination. Columns support signed 64-bit `INTEGER`, `REAL`, `TEXT`, `BOOLEAN`, and `BLOB`; large integers use `bigint`, and blobs use `Uint8Array`. Tables have a primary key or a persistent generated row ID.

The planner reads the transaction's actual index catalog and chooses primary-key or supported scalar secondary-index access. Other predicates use a table scan; ordering sorts the result in memory. SQL schema uses `_moyodb_sql_catalog` and a reserved ownership record in each table. Access tables through SQL so schema, ownership records, row encodings, and indexes remain consistent.

## Verification

\- Lean checks run with all built-in/extra linters, Heron, JunkLinter and the full Batteries set; every finding fails CI. Run `npm run lint:lean` locally.\
\- `proofs/moyodb_proofs` models abstract KV/WAL/transaction semantics and independently checks real inline B-tree pages. The B-tree checker has kernel-checked certificates for structure, contents and routing for every byte-string key; CI compares real Rust mutations, lookup/scan and retained COW roots with Lean-computed states and rejects two production mutation witnesses. This proves properties of the Lean checker and checks concrete Rust executions; the Rust engine, checksums, overflow, OPFS I/O, flush ordering and power loss remain outside those proofs.\
\- The traces exported to `proofs/artifacts` are replayed against the Rust engine by `crates/engine/tests/proof_artifacts.rs` as conformance scenarios.\
\- Crash behavior is covered by tests: `crates/engine/tests/crash_matrix.rs`, `crates/engine/tests/fault_injection.rs` (failed and short writes plus torn flushes at every WAL, main-file, and superblock operation during commit, checkpoint, and recovery), and `crates/engine/js/opfs_shim.test.mjs` for control-file publication.

## License

Copyright © 2026 Eluvane. The licensor is Eluvane; licensing requests go to [keiko1337@proton.me](mailto:keiko1337@proton.me). The official repository is [eluvane/MoyoDB](https://github.com/eluvane/MoyoDB).

MoyoDB **1.0.0** is the first release covered by the [Mother of Licenses 1.0](LICENSE) (`LicenseRef-MoL-1.0`). Earlier published releases retain their original licenses. Third-party dependencies and any separately licensed material retain their own licenses and notices.

MoL permits royalty-free use, including commercial use of unmodified code or Authorized Builds inside your own products. Private modifications and production patches are permitted for your own internal use, provided the modified code is not distributed, published, made available to third parties, or used to create a standalone or competing product or provide Project-as-a-Service. The upstream contribution workflow in Section 6 remains permitted. Other publication or distribution of modified versions, independent forks, resale of MoyoDB itself, and hosting MoyoDB itself as a service require written permission. MoyoDB under MoL is source-available, not OSI-approved open source.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) and Sections 9 and 10 of the license before submitting changes. The contribution and patent terms apply upon submission; no separate acceptance statement or PR checkbox is required by default.
