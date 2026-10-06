import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';

// Production SDK, registry, subscriptions, protocol and scheduler. Only the
// browser surfaces and WorkerApi storage boundary are deterministic doubles.
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const sourceIndex = args.indexOf('--source-root');
if (sourceIndex >= 0 && (!args[sourceIndex + 1] || args[sourceIndex + 1].startsWith('--'))) {
    throw new Error('missing --source-root value');
}
const source = resolve(sourceIndex < 0 ? join(here, '../src') : args[sourceIndex + 1]);
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
        nextTx = 1;
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
                    if (!databases.has(dbName)) databases.set(dbName, { version: 0, stores: new Set() });
                    this.database = databases.get(dbName);
                },
                close: async () => {
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
                        version: this.database.version
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
                calls.push({ worker: this, command: request.command });
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
        const result = ts.transpileModule(await readFile(join(source, `${name}.ts`), 'utf8'), {
            fileName: `${name}.ts`,
            reportDiagnostics: true,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
        });
        assert.equal(
            (result.diagnostics ?? []).filter((item) => item.category === ts.DiagnosticCategory.Error).length,
            0
        );
        await writeFile(join(output, `${name}.mjs`), result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'"));
    }
    const sdk = await import(pathToFileURL(join(output, 'index.mjs')).href);
    const { exposeWorkerApi } = await import(pathToFileURL(join(output, 'worker-server.mjs')).href);
    const protocol = await import(pathToFileURL(join(output, 'worker-protocol.mjs')).href);
    const tests = [];
    const test = (name, run) => tests.push({ name, run });
    const open = (name, options = {}) => sdk.openDB(name, { requestPersistence: false, ...options });
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
