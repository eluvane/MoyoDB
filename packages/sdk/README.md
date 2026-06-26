# @moyodb/sdk

TypeScript SDK for **MoyoDB**, an experimental Rust/WASM transactional key-value storage engine for browser apps.

The package name is lowercase/scoped for npm compatibility. The public brand is **MoyoDB**.

## Install

```bash
npm install @moyodb/sdk
```

This repository snapshot is prepared for `v0.0.0-alpha.1` package-readiness checks; publishing is not performed automatically.

## Usage

```ts
import { openDB, utf8Encode, utf8Decode } from '@moyodb/sdk';

const db = await openDB('app', { requestPersistence: false });
await db.createStore('kv');
await db.put('kv', utf8Encode('hello'), utf8Encode('world'));

const value = await db.get('kv', utf8Encode('hello'));
console.log(value ? utf8Decode(value) : null);
await db.close();
```

## Runtime requirements

The SDK expects a secure browser context with OPFS, Web Workers, BroadcastChannel, Web Locks, and Worker-side `FileSystemSyncAccessHandle` support. Unsupported browsers should receive a clear `UnsupportedPlatformError`.

## Local build

```bash
npm ci
npm run build:wasm
npm run typecheck
npm run build
```

## Benchmarks

```bash
npm run bench:browser
npm run bench:indexeddb
npm run bench:opfs
npm run bench:report
```

Native Rust microbenchmarks measure the engine core only. Browser benchmarks measure the SDK/WASM/Worker/OPFS path. IndexedDB comparison rows depend on browser, device, batch size, transaction boundaries, and workload.
