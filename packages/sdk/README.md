# @moyodb/sdk

TypeScript SDK for **MoyoDB**, an experimental Rust/WASM transactional key-value storage engine for browser apps.

The package name is lowercase/scoped for npm compatibility. The public brand is **MoyoDB**.

## Install

```bash
npm install @moyodb/sdk@1.0.0
```

Current release: `@moyodb/sdk@1.0.0`.

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

## License

Copyright © 2026 Eluvane. Licensor: Eluvane. Licensing contact: [keiko1337@proton.me](mailto:keiko1337@proton.me). Official repository: [eluvane/MoyoDB](https://github.com/eluvane/MoyoDB).

MoyoDB **1.0.0** is the first release covered by the Mother of Licenses 1.0 (`LicenseRef-MoL-1.0`); see the `LICENSE` file shipped with this package. Earlier published releases retain their original licenses. Third-party dependencies and separately licensed material retain their own licenses and notices.

Using unmodified SDK code or Authorized Builds inside your own applications, including commercial ones, is royalty-free. Private modifications and production patches are permitted for your own internal use, provided the modified code is not distributed, published, made available to third parties, or used to create a standalone or competing product or provide Project-as-a-Service. The upstream contribution workflow in Section 6 remains permitted; other publication or distribution of modified versions requires written permission. The full license governs redistribution, independent forks, resale, and hosted services.
