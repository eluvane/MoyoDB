import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';

// Tests production DbWorker and OwnershipLease with deterministic WASM and browser fixtures.
// Browser latency and OPFS durability are outside its scope.
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const flag = (name) => {
    const index = args.indexOf(name);
    if (index < 0) return undefined;
    if (!args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`missing ${name} value`);
    return args[index + 1];
};
const source = resolve(flag('--source-root') ?? join(here, '../src'));
const output = await mkdtemp(join(tmpdir(), 'moyo-startup-work-'));
const originalGlobals = new Map();
const setGlobal = (name, value) => {
    if (!originalGlobals.has(name)) originalGlobals.set(name, Object.getOwnPropertyDescriptor(globalThis, name));
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
};
const namedError = (name, message) => Object.assign(new Error(message), { name });
const deferred = () => {
    let resolvePromise;
    const promise = new Promise((resolve) => {
        resolvePromise = resolve;
    });
    return { promise, resolve: resolvePromise };
};

function createEnvironment(options = {}) {
    const events = [];
    const heldFileHandles = new Set();
    const heldLocks = new Set();
    const liveChannels = new Set();
    const listeners = new Set();
    const engines = [];
    let openedEngineHandles = 0;
    const event = (kind, details = {}) => events.push({ kind, ...details });
    class MockFile {
        constructor(path) {
            this.path = path;
        }
        async createSyncAccessHandle() {
            event('probe.createSyncAccessHandle', { path: this.path });
            if (heldFileHandles.has(this.path)) throw namedError('NoModificationAllowedError', 'file already locked');
            heldFileHandles.add(this.path);
            let closed = false;
            return {
                close: () => {
                    if (closed) return;
                    closed = true;
                    event('probe.close', { path: this.path });
                    heldFileHandles.delete(this.path);
                }
            };
        }
    }
    class MockDirectory {
        constructor(path = '') {
            this.path = path;
        }
        async getDirectoryHandle(name, flags) {
            event('probe.getDirectoryHandle', { name, ...flags });
            return new MockDirectory(`${this.path}/${name}`);
        }
        async getFileHandle(name, flags) {
            event('probe.getFileHandle', { name, ...flags });
            return new MockFile(`${this.path}/${name}`);
        }
        async removeEntry(name) {
            event('probe.removeEntry', { name });
        }
    }
    class MockBroadcastChannel {
        constructor(name) {
            this.name = name;
            liveChannels.add(this);
        }
        postMessage(message) {
            event('broadcast', { name: this.name, type: message.type });
        }
        close() {
            liveChannels.delete(this);
        }
    }
    const locks = {
        async request(name, _flags, callback) {
            event('lease.request', { name });
            if (options.leaseBusy || heldLocks.has(name)) return callback(null);
            heldLocks.add(name);
            event('lease.acquired', { name });
            try {
                return await callback({ name });
            } finally {
                heldLocks.delete(name);
                event('lease.released', { name });
            }
        }
    };
    class MockEngine {
        constructor() {
            this.handles = 0;
            engines.push(this);
            event('engine.construct');
        }
        async open(name, config) {
            event('engine.open', { name, config });
            if (!config.create_if_missing && options.missing)
                throw namedError('StorageError', 'database does not exist');
            this.handles = options.openFailure ? 1 : 3;
            openedEngineHandles += this.handles;
            event('engine.acquireHandles', { count: this.handles });
            if (options.openGate) await options.openGate.promise;
            if (options.openFailure) throw namedError('StorageError', 'injected real database open failure');
        }
        abandon() {
            event('engine.abandon');
            this.releaseHandles();
        }
        close() {
            event('engine.close');
            this.releaseHandles();
        }
        releaseHandles() {
            openedEngineHandles -= this.handles;
            this.handles = 0;
        }
    }
    const storage = {
        async getDirectory() {
            event('probe.getDirectory');
            return new MockDirectory();
        }
    };
    if (options.unsupported === 'getDirectory') storage.getDirectory = undefined;
    if (options.unsupported === 'syncHandle') delete MockFile.prototype.createSyncAccessHandle;
    setGlobal('navigator', { storage, locks });
    setGlobal('BroadcastChannel', options.unsupported === 'broadcast' ? undefined : MockBroadcastChannel);
    setGlobal('FileSystemFileHandle', MockFile);
    setGlobal('self', {
        isSecureContext: options.unsupported !== 'secureContext',
        location: { origin: 'https://startup-fixture.invalid' },
        addEventListener(_type, listener) {
            listeners.add(listener);
        },
        removeEventListener(_type, listener) {
            listeners.delete(listener);
        }
    });
    const wasm = {
        WasmEngine: MockEngine,
        async deleteDB(name) {
            event('engine.deleteDB', { name });
        }
    };
    return {
        events,
        wasm,
        engines,
        event,
        count: (kind) => events.filter((item) => item.kind === kind).length,
        probeCalls: () => events.filter((item) => item.kind.startsWith('probe.')).length,
        assertClean() {
            assert.equal(heldFileHandles.size, 0, 'probe handle leaked');
            assert.equal(openedEngineHandles, 0, 'database handle leaked');
            assert.equal(heldLocks.size, 0, 'ownership lease leaked');
            assert.equal(liveChannels.size, 0, 'BroadcastChannel leaked');
            assert.equal(listeners.size, 0, 'persistence bridge listener leaked');
        }
    };
}

const openRequest = (name, create = true) => ({
    dbName: name,
    options: {
        createIfMissing: create,
        ownerWaitMs: 0,
        requestPersistence: false,
        cachePages: 7,
        changeFeed: null,
        debugFailpoint: null
    }
});

try {
    await mkdir(output, { recursive: true });
    for (const name of ['worker', 'worker-protocol', 'indexing', 'codec', 'errors', 'internal', 'compression']) {
        const input = await readFile(join(source, `${name}.ts`), 'utf8');
        const transpiled = ts.transpileModule(input, {
            fileName: `${name}.ts`,
            reportDiagnostics: true,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
        });
        assert.equal(
            (transpiled.diagnostics ?? []).filter((d) => d.category === ts.DiagnosticCategory.Error).length,
            0
        );
        await writeFile(
            join(output, `${name}.mjs`),
            transpiled.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'")
        );
    }
    await writeFile(
        join(output, 'worker-server.mjs'),
        'export let runtime; export function exposeWorkerApi(api) { runtime = api; }\n'
    );
    const importEnvironment = createEnvironment();
    const { DbWorker: WorkerRuntime } = await import(pathToFileURL(join(output, 'worker.mjs')).href);
    importEnvironment.assertClean();

    async function fixture(options, body) {
        const env = createEnvironment(options);
        const runtimes = [];
        const makeRuntime = () => {
            const runtime = new WorkerRuntime({ loadWasm: async () => env.wasm });
            runtimes.push(runtime);
            return runtime;
        };
        try {
            return await body(env, makeRuntime);
        } finally {
            for (const runtime of runtimes) await runtime.close();
            env.assertClean();
        }
    }

    const observations = await fixture({}, async (env, makeRuntime) => {
        const runtime = makeRuntime();
        await runtime.open(openRequest('measurement'));
        const opened = {
            probeCalls: env.probeCalls(),
            engineOpenCalls: env.count('engine.open'),
            events: env.events.slice()
        };
        await runtime.close();
        const beforeDelete = env.events.length;
        await runtime.deleteDB('measurement');
        const deletion = env.events.slice(beforeDelete);
        return {
            open: opened,
            delete: {
                probeCalls: deletion.filter((e) => e.kind.startsWith('probe.')).length,
                engineDeleteCalls: deletion.filter((e) => e.kind === 'engine.deleteDB').length
            }
        };
    });
    const tests = [];
    const test = (name, body) => tests.push({ name, body });
    test('open forwards creation options and needs no synthetic file', async () => {
        for (const create of [true, false])
            await fixture({}, async (env, makeRuntime) => {
                await makeRuntime().open(openRequest(`create-${create}`, create));
                const calls = env.events.filter((item) => item.kind === 'engine.open');
                assert.equal(calls.length, 1);
                assert.deepEqual(calls[0].config, { create_if_missing: create, cache_pages: 7 });
                assert.equal(env.probeCalls(), 0, 'capability probing must use no synthetic file I/O');
            });
    });
    test('unsupported static primitives fail before storage or ownership', async () => {
        const messages = {
            secureContext: 'moyodb requires a secure context (HTTPS)',
            getDirectory: 'navigator.storage.getDirectory is unavailable',
            broadcast: 'BroadcastChannel is unavailable',
            syncHandle: 'createSyncAccessHandle is unavailable'
        };
        for (const unsupported of ['secureContext', 'getDirectory', 'broadcast', 'syncHandle']) {
            await fixture({ unsupported }, async (env, makeRuntime) => {
                await assert.rejects(makeRuntime().open(openRequest('unsupported')), {
                    name: 'UnsupportedPlatformError',
                    message: messages[unsupported]
                });
                assert.equal(env.count('lease.request'), 0);
                assert.equal(env.count('engine.construct'), 0);
                assert.equal(env.probeCalls(), 0, `${unsupported} detection must not create probe files`);
            });
        }
    });
    test('missing existing database preserves creation flag and releases ownership', async () => {
        await fixture({ missing: true }, async (env, makeRuntime) => {
            await assert.rejects(makeRuntime().open(openRequest('missing', false)), {
                name: 'StorageError',
                message: 'database does not exist'
            });
            assert.equal(env.count('engine.open'), 1);
            assert.equal(env.count('engine.acquireHandles'), 0);
            assert.equal(env.count('engine.abandon'), 1);
            assert.equal(env.count('lease.released'), 1);
        });
    });
    test('busy ownership fails without creating an engine', async () => {
        await fixture({ leaseBusy: true }, async (env, makeRuntime) => {
            await assert.rejects(makeRuntime().open(openRequest('busy')), { name: 'DatabaseBusyError' });
            assert.equal(env.count('engine.construct'), 0);
            assert.equal(env.count('engine.open'), 0);
        });
    });
    test('real open failure abandons acquired handles before releasing ownership', async () => {
        const gate = deferred();
        await fixture({ openFailure: true, openGate: gate }, async (env, makeRuntime) => {
            const operation = makeRuntime().open(openRequest('failing'));
            const rejected = assert.rejects(operation, {
                name: 'StorageError',
                message: 'injected real database open failure'
            });
            for (let turns = 0; turns < 100 && env.count('engine.acquireHandles') === 0; turns += 1)
                await Promise.resolve();
            assert.equal(env.count('engine.acquireHandles'), 1);
            assert.equal(env.count('lease.released'), 0);
            gate.resolve();
            await rejected;
            const abandon = env.events.findIndex((e) => e.kind === 'engine.abandon');
            const released = env.events.findIndex((e) => e.kind === 'lease.released');
            assert.ok(abandon >= 0 && released > abandon, 'close real handles before unlocking database');
        });
    });
    test('concurrent distinct databases do not contend on a synthetic probe', async () => {
        await fixture({}, async (env, makeRuntime) => {
            const results = await Promise.allSettled([
                makeRuntime().open(openRequest('distinct-a')),
                makeRuntime().open(openRequest('distinct-b'))
            ]);
            assert.deepEqual(
                results.map((r) => r.status),
                ['fulfilled', 'fulfilled']
            );
            assert.equal(env.count('engine.open'), 2);
            assert.equal(env.probeCalls(), 0);
        });
    });
    test('deleteDB retains one deletion and ownership lifecycle without a probe', async () => {
        await fixture({}, async (env, makeRuntime) => {
            await makeRuntime().deleteDB('delete-existing');
            assert.equal(env.count('engine.deleteDB'), 1);
            assert.equal(env.count('engine.construct'), 0);
            assert.equal(env.count('lease.acquired'), 1);
            assert.equal(env.count('lease.released'), 1);
            assert.equal(env.probeCalls(), 0);
        });
    });
    const results = [];
    if (!args.includes('--measure-only'))
        for (const item of tests) {
            try {
                await item.body();
                results.push({ name: item.name, passed: true });
            } catch (error) {
                results.push({ name: item.name, passed: false, error: error.message });
            }
        }
    const failed = results.filter((result) => !result.passed).length;
    console.log(
        JSON.stringify(
            {
                source,
                node: process.version,
                typescript: ts.version,
                level: 'portable startup work-accounting; mocked WASM/browser boundaries',
                observations,
                tests: results,
                passed: results.length - failed,
                failed
            },
            null,
            2
        )
    );
    if (failed > 0) process.exitCode = 1;
} finally {
    await rm(output, { recursive: true, force: true });
    for (const [name, descriptor] of originalGlobals) {
        if (descriptor) Object.defineProperty(globalThis, name, descriptor);
        else delete globalThis[name];
    }
}
