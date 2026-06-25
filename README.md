# MoyoDB

Experimental Rust/WASM transactional key-value storage engine for browser apps.

It provides ordered byte keys, range scans, snapshots, TTL, recovery checks, and a TypeScript SDK that talks to a dedicated Worker-backed runtime over OPFS.

> Status: **alpha / pre-public release**. MoyoDB is useful for experiments and early integrations. Do not use it for production data without your own durability and compatibility validation.

## Quick start

```bash
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-pack --version 0.15.0
npm ci
npm run build
npm run test:sdk
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

The npm package name is `@moyodb/sdk`. The human-facing project name is **MoyoDB**.

## Limitations

- Alpha software; storage format and SDK API may still change.
- Browser-only runtime; there is no Node.js persistence API.
- Requires OPFS and Worker-side sync access handles.
- One active owner per database and one write transaction at a time.
- Raw byte keys and values; no SQL/query planner.
- No cloud sync, multi-tab concurrent writers, SharedWorker runtime, or IndexedDB fallback.
