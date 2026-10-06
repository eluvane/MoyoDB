import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';
import { emitTranspiledModule, readFlagValue } from './fixture-emit.mjs';

// Production SDK, registry, subscriptions, protocol and scheduler. Only the
// browser surfaces and WorkerApi storage boundary are deterministic doubles.
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const source = resolve(readFlagValue(args, '--source-root') ?? join(here, '../src'));
const temporaryRoot = resolve(here, '../../../.tmp');
await mkdir(temporaryRoot, { recursive: true });
const output = await mkdtemp(join(temporaryRoot, 'sdk-lifecycle-'));
const originalGlobals = new Map();
const setGlobal = (name, value) => {
    if (!originalGlobals.has(name)) originalGlobals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
};
const namedError = (name) => Object.assign(new Error(name), { name });
const deferred = () => {
    let complete;
    const promise = new Promise((resolve) => {
        complete = resolve;
    });
    return { promise, resolve: complete };
};
const drain = async () => {
    for (let index = 0; index < 40; index += 1) await Promise.resolve();
};

class Surface {
    listeners = new Map();
    addEventListener(type, listener) {
        if (!this.listeners.has(type)) this.listeners.set(type, new Set());
        this.listeners.get(type).add(listener);
    }
    removeEventListener(type, listener) {
        this.listeners.get(type)?.delete(listener);
    }
    emit(type, data) {
        for (const listener of Array.from(this.listeners.get(type) ?? [])) listener({ data, origin: '' });
    }
}

function installEnvironment(exposeWorkerApi, protocol) {
    const workers = new Set();
    const owners = new Map();
    const databases = new Map();
    const channels = new Set();
    const gates = new Map();
    const calls = [];
    class MockPort extends Surface {
        closed = false;
        start() {}
        close() {
            this.closed = true;
        }
        postMessage(data) {
            queueMicrotask(() => {
                if (!this.peer.closed) this.peer.emit('message', data);
            });
        }
    }
    function MockMessageChannel() {
        const port1 = new MockPort();
        const port2 = new MockPort();
        port1.peer = port2;
        port2.peer = port1;
        return { port1, port2 };
    }
    class MockBroadcastChannel {
        constructor(name) {
            this.name = name;
            channels.add(this);
        }
        close() {
            channels.delete(this);
        }
    }
    class MockWorker extends Surface {
        requests = new Map();
        transactions = new Map();
        cursors = new Map();
        nextTx = 1;
        nextCursor = 1;
        terminated = false;
        constructor() {
            super();
            workers.add(this);
            const scope = new Surface();
            scope.location = { origin: 'https://sdk-lifecycle.test' };
            scope.postMessage = (message, transfer = []) => {
                const data = structuredClone(message, { transfer });
                const responses = data.type === protocol.WORKER_PROTOCOL_RESPONSE_BATCH ? data.responses : [data];
                for (const response of responses) {
                    const command = this.requests.get(response.id);
                    const gate = gates.get(command);
                    if (gate) {
                        gates.delete(command);
                        gate.response = () => this.emit('message', response);
                        gate.entered.resolve();
                    } else {
                        queueMicrotask(() => {
                            if (!this.terminated) this.emit('message', response);
                        });
                    }
                }
            };
            this.scope = scope;
            const requireTx = (id) => {
                const tx = this.transactions.get(id);
                if (!tx) throw namedError('TransactionClosedError');
                return tx;
            };
            const api = {
                open: async ({ dbName }) => {
                    if (owners.has(dbName)) throw namedError('DatabaseBusyError');
                    this.dbName = dbName;
                    owners.set(dbName, this);
                    if (!databases.has(dbName))
                        databases.set(dbName, {
                            version: 0,
                            stores: new Set(),
                            rows: Array.from({ length: 100 }, (_, index) => ({
                                key: Uint8Array.of(index + 1),
                                value: new Uint8Array(100).fill(index + 1)
                            }))
                        });
                    this.database = databases.get(dbName);
                },
                close: async () => {
                    this.cursors.clear();
                    this.transactions.clear();
                    if (owners.get(this.dbName) === this) owners.delete(this.dbName);
                },
                destroy: async () => {
                    await api.close();
                    databases.delete(this.dbName);
                },
                deleteDB: async (name) => {
                    if (owners.has(name)) throw namedError('DatabaseBusyError');
                    databases.delete(name);
                },
                begin: async (mode) => {
                    if (mode === 'readwrite' && [...this.transactions.values()].some((tx) => tx.mode === mode)) {
                        throw namedError('WriteTransactionAlreadyOpenError');
                    }
                    const id = this.nextTx++;
                    this.transactions.set(id, {
                        mode,
                        stores: new Set(this.database.stores),
                        snapshotStores: new Set(this.database.stores),
                        version: this.database.version,
                        rows: this.database.rows.slice()
                    });
                    return id;
                },
                commit: async (id) => {
                    const tx = requireTx(id);
                    this.database.stores = tx.stores;
                    this.database.version = tx.version;
                    this.transactions.delete(id);
                    return id;
                },
                rollback: async (id) => {
                    requireTx(id);
                    this.transactions.delete(id);
                    for (const [cursor, state] of this.cursors) if (state.txId === id) this.cursors.delete(cursor);
                },
                scanPage: async (request) => {
                    let cursorId = request.cursorId;
                    let state = this.cursors.get(cursorId);
                    if (cursorId !== undefined && !state) throw namedError('TransactionClosedError');
                    if (!state) {
                        const txId = request.txId ?? (await api.begin('readonly'));
                        state = { txId, ownsTx: request.txId === undefined, offset: 0, rows: requireTx(txId).rows };
                        cursorId = this.nextCursor++;
                        this.cursors.set(cursorId, state);
                    }
                    const rows = [];
                    let bytes = 4;
                    while (state.offset < state.rows.length && rows.length < request.maxRows) {
                        const row = state.rows[state.offset];
                        const size = 8 + row.key.byteLength + row.value.byteLength;
                        if (bytes + size > request.maxBytes) {
                            if (!rows.length) {
                                await api.closeCursor(cursorId);
                                throw namedError('ValueTooLargeError');
                            }
                            break;
                        }
                        rows.push(row);
                        bytes += size;
                        state.offset += 1;
                    }
                    const done = state.offset === state.rows.length;
                    if (done) await api.closeCursor(cursorId);
                    return { rows, cursorId: done ? undefined : cursorId, done, bytes };
                },
                closeCursor: async (id) => {
                    const state = this.cursors.get(id);
                    this.cursors.delete(id);
                    if (state?.ownsTx) this.transactions.delete(state.txId);
                },
                scanByIndexPage: async (id, _store, _index, _range, cursor, limit, maxBytes) => {
                    const available = requireTx(id).rows.filter((row) => !cursor || row.key[0] > cursor[0]);
                    const rows = [];
                    let bytes = 4;
                    for (const row of available) {
                        const size = 8 + row.key.byteLength + row.value.byteLength;
                        if (rows.length === limit || bytes + size > maxBytes) break;
                        rows.push(row);
                        bytes += size;
                    }
                    if (!rows.length && available.length) throw namedError('ValueTooLargeError');
                    return { rows, cursor: rows.length === available.length ? null : rows.at(-1).key };
                },
                get: async (id) => {
                    requireTx(id);
                    return null;
                },
                createStore: async (id, name) => {
                    const tx = requireTx(id);
                    if (tx.stores.has(name) || tx.snapshotStores.has(name)) throw namedError('StoreExistsError');
                    tx.stores.add(name);
                },
                dropStore: async (id, name) => {
                    if (!requireTx(id).stores.delete(name)) throw namedError('StoreNotFoundError');
                },
                getIndexes: async () => [],
                reconcileIndexes: async (id) => {
                    requireTx(id);
                },
                getVersion: async () => this.database.version,
                setSchemaVersion: async (id, version) => {
                    requireTx(id).version = version;
                },
                listStores: async () => [...this.database.stores].sort(),
                importSnapshot: async () => this.transactions.clear(),
                reset: async () => this.transactions.clear(),
                setFailpoint: async () => {}
            };
            this.server = exposeWorkerApi(api, scope);
        }
        postMessage(message, transfer = []) {
            if (message.type === 'moyodb:persistence-bridge:init') {
                this.persistencePort = transfer[0];
                return;
            }
            const data = structuredClone(message, { transfer });
            for (const request of data.requests ?? [data]) {
                this.requests.set(request.id, request.command);
                calls.push({ worker: this, command: request.command, args: request.args });
            }
            queueMicrotask(() => this.scope.emit('message', data));
        }
        terminate() {
            this.terminated = true;
            this.server.close();
            this.persistencePort?.close();
            this.transactions.clear();
            if (owners.get(this.dbName) === this) owners.delete(this.dbName);
            workers.delete(this);
        }
    }
    setGlobal('isSecureContext', true);
    setGlobal('navigator', { storage: { getDirectory: async () => ({}) } });
    setGlobal('MessageChannel', MockMessageChannel);
    setGlobal('Worker', MockWorker);
    setGlobal('BroadcastChannel', MockBroadcastChannel);
    return {
        workers,
        calls,
        activeTransactions: () => [...workers].reduce((count, worker) => count + worker.transactions.size, 0),
        holdResponse(command) {
            const gate = { entered: deferred(), response: null };
            gates.set(command, gate);
            return { entered: gate.entered.promise, release: () => gate.response?.() };
        },
        broadcast(payload) {
            for (const channel of Array.from(channels)) channel.onmessage?.({ data: structuredClone(payload) });
        }
    };
}

try {
    for (const name of [
        'index',
        'registry',
        'subscriptions',
        'change-events',
        'worker-client',
        'shared-worker-client',
        'shared-worker-protocol',
        'worker-server',
        'worker-protocol',
        'codec',
        'indexing',
        'records',
        'sql',
        'sql-parser',
        'sql-types',
        'errors',
        'internal'
    ]) {
        await emitTranspiledModule({
            name,
            input: join(source, `${name}.ts`),
            outputDir: output,
            reportErrors: 'assert'
        });
    }
    const sdk = await import(pathToFileURL(join(output, 'index.mjs')).href);
    const { exposeWorkerApi } = await import(pathToFileURL(join(output, 'worker-server.mjs')).href);
    const protocol = await import(pathToFileURL(join(output, 'worker-protocol.mjs')).href);
    const tests = [];
    const test = (name, run) => tests.push({ name, run });
    const open = (name, options = {}) => sdk.openDB(name, { requestPersistence: false, ...options });
    test('database names reject every unpaired surrogate before creating worker state', async (env) => {
        const aliases = ['db:\uD800', 'db:\uDC00', 'db:\uFFFD'];
        assert.equal(new Set(aliases).size, 3);
        for (const alias of aliases)
            assert.deepEqual(new TextEncoder().encode(alias), new TextEncoder().encode(aliases[2]));
        const malformed = Array.from({ length: 0x800 }, (_, index) => String.fromCharCode(0xd800 + index));
        malformed.push('\uD800x', 'x\uDC00', '\uDC00\uD800', '\uD800\uD800\uDC00', '\uD800\uDC00\uDC00');
        for (const name of malformed) {
            await assert.rejects(open(name), {
                name: 'TypeError',
                message: 'openDB() database name must contain valid Unicode'
            });
            await assert.rejects(sdk.deleteDB(name), {
                name: 'TypeError',
                message: 'deleteDB() database name must contain valid Unicode'
            });
            assert.throws(() => sdk.unsafeDebugCrashWorker(name), {
                name: 'TypeError',
                message: 'unsafeDebugCrashWorker() database name must contain valid Unicode'
            });
        }
        assert.equal(env.workers.size, 0);
        assert.equal(env.calls.length, 0);
    });
    test('database names preserve valid Unicode and enforce the UTF-8 byte boundary', async (env) => {
        const names = ['\uFFFD', '\uD800\uDC00', '\uDBFF\uDFFF', 'я', 'é', 'e\u0301', '😀'.repeat(31) + 'abc'];
        const handles = [];
        try {
            for (const name of names) {
                handles.push(await open(name));
            }
            assert.equal(sdk.unsafeDebugCrashWorker('valid-unopened-\uD800\uDC00-\uFFFD'), false);
            assert.equal(new TextEncoder().encode(names.at(-1)).length, 127);
            assert.equal(env.workers.size, names.length);
            assert.deepEqual(
                env.calls.filter((call) => call.command === 'open').map((call) => call.args[0].dbName),
                names
            );
            const before = env.calls.length;
            for (const name of ['x'.repeat(128), 'é'.repeat(64), '😀'.repeat(32)]) {
                await assert.rejects(open(name), /at most 127 UTF-8 bytes/);
                await assert.rejects(sdk.deleteDB(name), /at most 127 UTF-8 bytes/);
                assert.throws(() => sdk.unsafeDebugCrashWorker(name), /at most 127 UTF-8 bytes/);
            }
            assert.equal(env.calls.length, before);
        } finally {
            await Promise.all(handles.map((handle) => handle.close()));
        }
        for (const name of names) await sdk.deleteDB(name);
        assert.deepEqual(
            env.calls.filter((call) => call.command === 'deleteDB').map((call) => call.args[0]),
            names
        );
        assert.equal(env.workers.size, 0);
    });
    test('primary scan pages keep one snapshot and obey row and byte budgets', async (env) => {
        const db = await open('cursor-snapshot');
        const first = await db.scanPage('kv', {}, { maxRows: 2, maxBytes: 128 });
        assert.equal(first.rows.length, 1);
        assert.equal(first.bytes, 113);
        assert.equal(env.activeTransactions(), 1);
        const worker = [...env.workers][0];
        worker.database.rows = [];
        const second = await db.scanPage('kv', {}, { cursor: first.cursor, maxRows: 2, maxBytes: 256 });
        assert.deepEqual(
            second.rows.map((row) => row.key[0]),
            [2, 3]
        );
        await db.closeScanCursor(second.cursor);
        assert.equal(env.activeTransactions(), 0);
        assert.equal(worker.cursors.size, 0);
        await db.close();
    });
    test('breaking primary iteration releases its snapshot without another page', async (env) => {
        const db = await open('cursor-break');
        for await (const row of db.scanIter('kv', {}, { maxRows: 1 })) {
            assert.equal(row.key[0], 1);
            break;
        }
        assert.equal(env.calls.filter((call) => call.command === 'scanPage').length, 1);
        assert.equal(env.calls.filter((call) => call.command === 'closeCursor').length, 1);
        assert.equal(env.activeTransactions(), 0);
        await db.close();
    });
    test('a primary cursor belongs to its database handle or transaction', async (env) => {
        const first = await open('cursor-owner');
        const second = await open('cursor-owner');
        const page = await first.scanPage('kv', {}, { maxRows: 1 });
        const before = env.calls.length;
        await assert.rejects(second.scanPage('kv', {}, { cursor: page.cursor }), /does not belong/);
        assert.equal(env.calls.length, before);
        await first.close();
        assert.equal(env.activeTransactions(), 0);
        const tx = await second.begin('readonly');
        const txPage = await tx.scanPage('kv', {}, { maxRows: 1 });
        await tx.rollback();
        await assert.rejects(tx.scanPage('kv', {}, { cursor: txPage.cursor }), { name: 'TransactionClosedError' });
        assert.equal(env.activeTransactions(), 0);
        await second.close();
    });
    test('close consumes an in-flight primary scan reply on a shared handle', async (env) => {
        const first = await open('pending-scan');
        const second = await open('pending-scan');
        const gate = env.holdResponse('scanPage');
        const pending = first.scanPage('kv', {}, { maxRows: 1 });
        const settled = Promise.allSettled([pending]);
        await gate.entered;
        const closing = first.close();
        gate.release();
        await closing;
        const [result] = await settled;
        assert.equal(result.status, 'rejected');
        assert.equal(result.reason.name, 'DatabaseClosedError');
        assert.equal(env.activeTransactions(), 0);
        assert.equal([...env.workers][0].cursors.size, 0);
        await second.close();
    });
    test('an oversized first primary row closes its snapshot', async (env) => {
        const db = await open('cursor-oversize');
        await assert.rejects(db.scanPage('kv', {}, { maxRows: 1, maxBytes: 100 }), { name: 'ValueTooLargeError' });
        assert.equal(env.activeTransactions(), 0);
        await db.close();
    });
    test('default primary page accepts the largest supported payload and key', async (env) => {
        const db = await open('cursor-largest');
        [...env.workers][0].database.rows = [{ key: new Uint8Array(1024), value: new Uint8Array(8 * 1024 * 1024) }];
        const page = await db.scanPage('kv');
        assert.equal(page.rows.length, 1);
        assert.equal(page.bytes, 8 * 1024 * 1024 + 1024 + 12);
        assert.equal(page.done, true);
        assert.equal(page.cursor, undefined);
        assert.equal(env.calls.findLast((call) => call.command === 'scanPage').args[0].maxBytes, page.bytes + 18);
        assert.equal(env.activeTransactions(), 0);
        const before = env.calls.length;
        await assert.rejects(db.scanPage('kv', {}, { maxRows: 0 }), /maxRows/);
        await assert.rejects(db.scanPage('kv', {}, { maxBytes: 3 }), /maxBytes/);
        assert.equal(env.calls.length, before);
        await db.close();
    });
    test('secondary iteration forwards row and decoded byte budgets on every page', async (env) => {
        const db = await open('index-cursor-budget');
        const tx = await db.begin('readonly');
        const keys = [];
        for await (const [key] of tx.scanByIndex('kv', 'by_value', { limit: 3 }, { maxRows: 10, maxBytes: 128 }))
            keys.push(key[0]);
        assert.deepEqual(keys, [1, 2, 3]);
        const calls = env.calls.filter((call) => call.command === 'scanByIndexPage');
        assert.equal(calls.length, 3);
        assert.ok(calls.every((call) => call.args[6] === 128));
        assert.deepEqual(
            calls.map((call) => call.args[5]),
            [3, 2, 1]
        );
        await assert.rejects(
            async () => {
                for await (const row of tx.scanByIndex('kv', 'by_value', {}, { maxBytes: 100 })) {
                    assert.fail(`oversized indexed row returned ${row.key.length} key bytes`);
                }
            },
            { name: 'ValueTooLargeError' }
        );
        await tx.rollback();
        await db.close();
    });
    test('concurrent same-name opens share one owner and independent handles', async (env) => {
        const results = await Promise.allSettled([open('shared-open'), open('shared-open')]);
        assert.ok(
            results.every((result) => result.status === 'fulfilled'),
            'same-tab opens must share the owner'
        );
        assert.equal(env.workers.size, 1);
        await results[0].value.close();
        assert.deepEqual(await results[1].value.listStores(), []);
        await results[1].value.close();
        assert.equal(env.workers.size, 0);
    });
    test('close consumes an in-flight begin on a shared handle', async (env) => {
        const first = await open('pending-begin');
        const second = await open('pending-begin');
        const gate = env.holdResponse('begin');
        const pending = first.begin('readwrite');
        const settled = Promise.allSettled([pending]);
        await gate.entered;
        const closing = first.close();
        gate.release();
        await closing;
        const result = (await settled)[0];
        assert.equal(result.status, 'rejected');
        assert.equal(result.reason.name, 'DatabaseClosedError');
        assert.equal(env.activeTransactions(), 0, 'closed handle must release its pending writer');
        const writer = await second.begin('readwrite');
        await writer.rollback();
        await second.close();
    });
    test('every close waits for the same cleanup', async (env) => {
        const db = await open('repeat-close');
        const gate = env.holdResponse('close');
        const first = db.close();
        await gate.entered;
        let completed = false;
        const repeated = db.close().then(() => {
            completed = true;
            return undefined;
        });
        await drain();
        const completedBeforeCleanup = completed;
        gate.release();
        await Promise.all([first, repeated]);
        assert.equal(completedBeforeCleanup, false);
        assert.equal(env.workers.size, 0);
    });
    test('snapshot import invalidates transaction objects locally', async (env) => {
        const db = await open('import-invalidates');
        const tx = await db.begin('readonly');
        await db.importSnapshot(new Uint8Array());
        const before = env.calls.length;
        await assert.rejects(tx.get('kv', new Uint8Array()), { name: 'TransactionClosedError' });
        assert.equal(env.calls.length, before, 'invalidated transaction must not issue another worker command');
        await db.close();
    });
    test('maintenance rejects delayed transaction replies', async (env) => {
        const db = await open('maintenance-begin');
        const gate = env.holdResponse('begin');
        const pending = db.begin('readonly');
        const settled = Promise.allSettled([pending]);
        await gate.entered;
        await db.reset();
        gate.release();
        const result = (await settled)[0];
        await db.close();
        assert.equal(result.status, 'rejected');
        assert.equal(result.reason.name, 'TransactionClosedError');
    });
    test('closing from a subscription stops the current delivery', async (env) => {
        const db = await open('subscription-close');
        let closing;
        const delivered = [];
        db.subscribe((store) => {
            delivered.push(`first:${store}`);
            closing = db.close();
        });
        db.subscribe((store) => delivered.push(`second:${store}`));
        env.broadcast({
            type: 'commit_applied',
            dbName: 'subscription-close',
            txid: 1,
            stores: ['a', 'b'].map((store) => ({ store, changes: [{ key: new Uint8Array([1]), kind: 'put' }] }))
        });
        await closing;
        assert.deepEqual(delivered, ['first:a']);
    });
    test('migration catalog respects create-drop-recreate-drop of a new store', async () => {
        const base = await open('migration-catalog');
        await base.createStore('existing');
        await base.close();
        let observed;
        const upgraded = await open('migration-catalog', {
            version: 1,
            migrate: async ({ db }) => {
                await db.createStore('temporary');
                await db.dropStore('temporary');
                await db.createStore('temporary');
                await db.dropStore('temporary');
                observed = await db.listStores();
            }
        });
        await upgraded.close();
        assert.deepEqual(observed, ['existing']);
    });
    test('delete waits for a pending open before deleting its owner', async (env) => {
        const gate = env.holdResponse('open');
        const opening = open('delete-opening');
        await gate.entered;
        const deleting = sdk.deleteDB('delete-opening');
        const settled = Promise.allSettled([opening, deleting]);
        gate.release();
        const [opened, deleted] = await settled;
        assert.equal(opened.status, 'fulfilled');
        assert.equal(deleted.status, 'fulfilled');
        await assert.rejects(opened.value.listStores(), { name: 'DatabaseClosedError' });
        assert.equal(env.workers.size, 0);
    });
    test('overlapping writers surface WriteTransactionAlreadyOpenError', async () => {
        const db = await open('two-writers');
        const writer = await db.begin('readwrite');
        await assert.rejects(db.begin('readwrite'), { name: 'WriteTransactionAlreadyOpenError' });
        await writer.rollback();
        await db.close();
    });
    test('opening another handle cannot bypass an in-progress migration', async () => {
        const entered = deferred();
        const gate = deferred();
        const opening = open('migration-owner', {
            version: 1,
            migrate: async () => {
                entered.resolve();
                await gate.promise;
            }
        });
        await entered.promise;
        const contender = await Promise.allSettled([open('migration-owner')]);
        if (contender[0].status === 'fulfilled') await contender[0].value.close();
        gate.resolve();
        const db = await opening;
        await db.close();
        assert.equal(contender[0].status, 'rejected');
        assert.equal(contender[0].reason.name, 'DatabaseBusyError');
    });
    let passed = 0;
    let failed = 0;
    for (const { name, run } of tests) {
        const env = installEnvironment(exposeWorkerApi, protocol);
        try {
            await run(env);
            console.log(`PASS ${name}`);
            passed += 1;
        } catch (error) {
            console.error(`FAIL ${name}: ${error.stack ?? error}`);
            failed += 1;
        } finally {
            for (const worker of Array.from(env.workers)) {
                if (worker.dbName) sdk.unsafeDebugCrashWorker(worker.dbName);
                if (!worker.terminated) worker.terminate();
            }
        }
    }
    console.log(JSON.stringify({ source, node: process.version, typescript: ts.version, passed, failed }, null, 2));
    if (failed > 0) process.exitCode = 1;
} finally {
    for (const [name, descriptor] of originalGlobals) {
        if (descriptor) Object.defineProperty(globalThis, name, descriptor);
        else Reflect.deleteProperty(globalThis, name);
    }
    const cleanupOutput = resolve(output);
    assert.equal(dirname(cleanupOutput), temporaryRoot, 'temporary output escaped the lifecycle fixture root');
    await rm(cleanupOutput, { recursive: true, force: true });
}
