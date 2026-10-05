import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { MessageChannel } from 'node:worker_threads';
import ts from 'typescript';

const here = dirname(fileURLToPath(import.meta.url));
const source = join(here, '../src');
const output = await mkdtemp(join(tmpdir(), 'moyo-worker-portability-'));
const originals = new Map();
const setGlobal = (name, value) => {
    if (!originals.has(name)) originals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
};
const noPersistence = () => ({ persisted: async () => false, persist: async () => false, close() {} });
const observations = [];

try {
    for (const name of [
        'worker',
        'browser-worker',
        'worker-server',
        'worker-protocol',
        'worker-client',
        'indexing',
        'codec',
        'errors',
        'internal',
        'compression'
    ]) {
        const result = ts.transpileModule(await readFile(join(source, `${name}.ts`), 'utf8'), {
            fileName: `${name}.ts`,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
            reportDiagnostics: true
        });
        assert.equal(
            (result.diagnostics ?? []).filter((diagnostic) => diagnostic.category === ts.DiagnosticCategory.Error)
                .length,
            0
        );
        await writeFile(join(output, `${name}.mjs`), result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'"));
    }

    setGlobal('self', undefined);
    const { DbWorker, exposeWorkerApi } = await import(pathToFileURL(join(output, 'worker.mjs')).href);
    const { WorkerProtocolClient } = await import(pathToFileURL(join(output, 'worker-client.mjs')).href);
    const protocol = await import(pathToFileURL(join(output, 'worker-protocol.mjs')).href);
    const indexing = await import(pathToFileURL(join(output, 'indexing.mjs')).href);
    observations.push('runtime imports without a browser scope');

    let closed = 0;
    const persistenceFailure = new Error('persistence unavailable');
    const runtime = new DbWorker({
        persistence: {
            persisted: async () => true,
            persist: async () => {
                throw persistenceFailure;
            },
            close() {
                closed += 1;
            }
        }
    });
    await assert.rejects(runtime.requestPersistence(), (error) => error === persistenceFailure);
    await runtime.close();
    assert.equal(closed, 1);
    observations.push('injected persistence preserves failures and closes without browser listeners');

    setGlobal('self', { isSecureContext: true });
    setGlobal('navigator', {
        storage: { getDirectory() {} },
        locks: {
            async request(_name, _options, callback) {
                return callback({ name: _name });
            }
        }
    });
    setGlobal(
        'FileSystemFileHandle',
        class {
            createSyncAccessHandle() {}
        }
    );
    let loads = 0;
    const deleted = [];
    const loadedRuntime = new DbWorker({
        loadWasm: async () => {
            loads += 1;
            return {
                async deleteDB(name) {
                    deleted.push(name);
                }
            };
        },
        persistence: noPersistence()
    });
    await loadedRuntime.deleteDB('portable-first');
    await loadedRuntime.deleteDB('portable-second');
    await loadedRuntime.close();
    assert.equal(loads, 1);
    assert.deepEqual(deleted, ['portable-first', 'portable-second']);
    observations.push('injected WASM loader is reused across public maintenance requests');

    const catalogRuntime = new DbWorker({ persistence: noPersistence() });
    const definitions = indexing.normalizeIndexDefinitions([{ store: 'docs', name: 'by_age', keyPath: 'age' }]);
    const expectedIndexes = indexing.toPublicIndexDefinitions(definitions);
    let nextTxId = 1n;
    catalogRuntime.committedIndexes = definitions;
    catalogRuntime.committedStoreCompression = new Map();
    catalogRuntime.engine = {
        needs_recovery: () => false,
        begin_tx: () => nextTxId++,
        rollback_tx() {},
        commit_tx: () => 1n,
        create_store() {},
        clear_store() {},
        drop_store() {},
        scan(_txId, store, range) {
            return store === indexing.INDEX_METADATA_STORE && range.limit !== 0
                ? definitions.map((definition) => ({
                      key: indexing.encodeIndexMetadataKey(definition.store, definition.name),
                      value: indexing.encodeIndexMetadataValue(definition)
                  }))
                : [];
        },
        close() {}
    };
    const reader = await catalogRuntime.begin('readonly');
    const writer = await catalogRuntime.begin('readwrite');
    await catalogRuntime.reconcileIndexes(writer, []);
    assert.deepEqual(await catalogRuntime.getIndexes(writer), []);
    assert.deepEqual(await catalogRuntime.getIndexes(reader), expectedIndexes);
    assert.deepEqual(await catalogRuntime.getIndexes(), expectedIndexes);
    await catalogRuntime.commit(writer);
    assert.deepEqual(await catalogRuntime.getIndexes(), []);
    assert.deepEqual(await catalogRuntime.getIndexes(reader), expectedIndexes);
    await catalogRuntime.rollback(reader);
    await catalogRuntime.close();
    observations.push('index catalog reads retain transaction schema after another transaction commits');

    const bootstrapListeners = new Set();
    const bootstrapMessages = [];
    setGlobal('self', {
        location: { origin: 'https://portable.test' },
        addEventListener(_type, listener) {
            bootstrapListeners.add(listener);
        },
        removeEventListener(_type, listener) {
            bootstrapListeners.delete(listener);
        },
        postMessage(message) {
            bootstrapMessages.push(message);
        }
    });
    await import(pathToFileURL(join(output, 'browser-worker.mjs')).href);
    assert.equal(bootstrapListeners.size, 3);
    const bootstrapPorts = new MessageChannel();
    bootstrapPorts.port2.start();
    const bootstrapClient = new WorkerProtocolClient(bootstrapPorts.port2, {
        readyTimeoutMs: 1000,
        requestTimeoutMs: 1000
    });
    for (const listener of Array.from(bootstrapListeners)) {
        listener({ origin: '', data: { type: 'moyodb:worker-port:init' }, ports: [bootstrapPorts.port1] });
    }
    try {
        await bootstrapClient.whenReady();
        assert.equal(bootstrapListeners.size, 2);
        await assert.rejects(bootstrapClient.getVersion(), { name: 'InternalError' });
        await bootstrapClient.close();
        assert.equal(bootstrapListeners.size, 1);
        assert.equal(bootstrapMessages.length, 1);
        observations.push(
            'browser bootstrap moves one runtime to a native MessagePort and retains persistence cleanup'
        );
    } finally {
        bootstrapClient.dispose();
        bootstrapPorts.port1.close();
        bootstrapPorts.port2.close();
    }

    const ports = new MessageChannel();
    const scope = {
        location: { origin: 'https://portable.test' },
        postMessage: (message, transfer = []) => ports.port1.postMessage(message, transfer),
        addEventListener: (type, listener) => ports.port1.addEventListener(type, listener),
        removeEventListener: (type, listener) => ports.port1.removeEventListener(type, listener)
    };
    ports.port1.start();
    ports.port2.start();
    const client = new WorkerProtocolClient(ports.port2, { readyTimeoutMs: 1000, requestTimeoutMs: 1000 });
    const serverError = Object.assign(new Error('stale writer'), { name: 'TransactionConflictError' });
    const returnedBuffers = [];
    const server = exposeWorkerApi(
        {
            async listStores() {
                return ['portable'];
            },
            async get(_txId, _store, key) {
                const value = key.slice();
                returnedBuffers.push(value.buffer);
                return value;
            },
            async commit() {
                throw serverError;
            },
            async getIndexes(txId) {
                return txId === undefined ? [] : expectedIndexes;
            }
        },
        scope
    );
    try {
        await client.whenReady();
        assert.deepEqual(await client.listStores(), ['portable']);
        assert.deepEqual(await client.getIndexes(), []);
        assert.deepEqual(await client.getIndexes(1), expectedIndexes);
        const key = new Uint8Array([1, 2, 3]);
        const [first, second] = await Promise.all([client.get(1, 'kv', key), client.get(2, 'kv', key)]);
        assert.deepEqual(Array.from(first), [1, 2, 3]);
        assert.deepEqual(Array.from(second), [1, 2, 3]);
        assert.deepEqual(Array.from(key), [1, 2, 3]);
        assert.ok(returnedBuffers.every((buffer) => buffer.byteLength === 0));
        await assert.rejects(client.commit(1), { name: 'TransactionConflictError', message: 'stale writer' });
        assert.deepEqual(await client.listStores(), ['portable']);
        observations.push('native MessagePort transport preserves binary ownership and recoverable errors');
    } finally {
        client.dispose();
        server.close();
        ports.port1.close();
        ports.port2.close();
    }

    for (const invalid of [
        { type: protocol.WORKER_PROTOCOL_READY, version: 2 },
        { type: protocol.WORKER_PROTOCOL_READY, version: 1, batching: 2 }
    ]) {
        const channel = new MessageChannel();
        channel.port2.start();
        const incompatible = new WorkerProtocolClient(channel.port2, { readyTimeoutMs: 1000 });
        const pending = incompatible.listStores();
        const rejected = Promise.all([
            assert.rejects(incompatible.whenReady(), { name: 'WorkerProtocolError' }),
            assert.rejects(pending, { name: 'WorkerProtocolError' })
        ]);
        channel.port1.postMessage(invalid);
        try {
            await rejected;
        } finally {
            incompatible.dispose();
            channel.port1.close();
            channel.port2.close();
        }
    }
    observations.push('incompatible ready version or capability rejects pending requests');
    console.log(JSON.stringify({ node: process.version, passed: observations.length, observations }, null, 2));
} finally {
    for (const [name, descriptor] of originals) {
        if (descriptor) Object.defineProperty(globalThis, name, descriptor);
        else delete globalThis[name];
    }
    await rm(output, { recursive: true, force: true });
}
