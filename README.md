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

> Status: **alpha**. MoyoDB is useful for experiments and early integrations. Do not use it for production data without your own durability and compatibility validation.

## Quick start

```bash
npm install @moyodb/sdk@alpha
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

The current alpha package is `@moyodb/sdk@0.0.0-alpha.2`. The human-facing project name is **MoyoDB**.

## Limitations

- Alpha software; storage format and SDK API may still change.
- Browser-only runtime; there is no Node.js persistence API.
- Requires OPFS and Worker-side sync access handles.
- One active owner per database and one write transaction at a time.
- Raw byte keys and values; no SQL/query planner.
- No cloud sync, multi-tab concurrent writers, SharedWorker runtime, or IndexedDB fallback.
