import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';

// Stock mode uses unchanged browser workload runners for A/B comparisons.
// Diagnostic instrumentation changes timings.
const here = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(here, '../../..');
const defaultWorkloads = [
    'open_empty_db',
    'point_get_random_10k_pipelined',
    'reverse_scan_limit_1',
    'small_tx_1000_commits',
    'large_value_64kb',
    'large_value_1mb'
];
const { values } = parseArgs({
    allowNegative: true,
    options: {
        'source-root': { type: 'string', default: repositoryRoot },
        'dependency-root': { type: 'string', default: repositoryRoot },
        url: { type: 'string', default: 'http://127.0.0.1:4173/bench/browser-bench.html' },
        label: { type: 'string', default: 'unlabelled' },
        sha: { type: 'string' },
        out: { type: 'string' },
        engines: { type: 'string', default: 'moyodb,indexeddb' },
        workloads: { type: 'string', default: defaultWorkloads.join(',') },
        'large-value-sizes': { type: 'string' },
        'total-bytes': { type: 'string', default: '67108864' },
        'batch-bytes': { type: 'string', default: '8388608' },
        samples: { type: 'string', default: '3' },
        warmups: { type: 'string', default: '1' },
        'timeout-ms': { type: 'string', default: '300000' },
        diagnostic: { type: 'boolean', default: false },
        'engine-timings': { type: 'boolean', default: false },
        'reopen-check': { type: 'boolean', default: false },
        'keep-profile': { type: 'boolean', default: false },
        serve: { type: 'boolean', default: true },
        help: { type: 'boolean', default: false }
    }
});
if (values.help) {
    console.log(`Usage: node packages/sdk/scripts/bench-performance-gap.mjs --source-root REPO --sha SHA --label before --out RAW.json
  --url http://127.0.0.1:4173/bench/browser-bench.html
  --dependency-root REPO_WITH_INSTALLED_PLAYWRIGHT
  --samples 3 --warmups 1 --engines moyodb,indexeddb --workloads ${defaultWorkloads.join(',')}
  --large-value-sizes 4096,16384,65536,262144,1048576
                                  Run only symmetric scaling specs; stock runners unchanged
  --total-bytes 67108864 --batch-bytes 8388608
  --diagnostic [--engine-timings]   Separate, instrumented diagnostic run
  --reopen-check                  Separate post-suite content + close/reopen probe
  --keep-profile                  Retain temporary browser profile for inspection
  --no-serve                      Use a server already reachable by this process
  MOYO_CHROMIUM_EXECUTABLE        Optional Chromium executable path
The caller must build release WASM before invoking this script.`);
    process.exit(0);
}

function count(option, minimum) {
    const n = Number(values[option]);
    assert.ok(Number.isSafeInteger(n) && n >= minimum, `invalid --${option}`);
    return n;
}
const sourceRoot = resolve(values['source-root']);
const engines = values.engines
    .split(',')
    .map((x) => x.trim())
    .filter(Boolean);
assert.ok(engines.length > 0 && engines.every((x) => x === 'moyodb' || x === 'indexeddb'));
assert.equal(new Set(engines).size, engines.length);
let workloadNames = values.workloads
    .split(',')
    .map((x) => x.trim())
    .filter(Boolean);
const scalingSpecs = [];
if (values['large-value-sizes']) {
    const totalBytes = count('total-bytes', 1);
    const batchBytes = count('batch-bytes', 1);
    assert.equal(totalBytes % batchBytes, 0, 'scaling total bytes must be a multiple of batch bytes');
    const sizes = values['large-value-sizes'].split(',').map(Number);
    assert.equal(new Set(sizes).size, sizes.length, 'duplicate scaling sizes');
    for (const valueSize of sizes) {
        assert.ok(
            Number.isSafeInteger(valueSize) && valueSize > 0 && valueSize <= batchBytes,
            'invalid scaling value size'
        );
        assert.equal(batchBytes % valueSize, 0, 'each scaling value size must divide the per-transaction payload');
        scalingSpecs.push({
            name: `large_value_scaling_${valueSize}b`,
            recordCount: totalBytes / valueSize,
            valueSize,
            batchSize: batchBytes / valueSize,
            transactionBoundaries: `${totalBytes / batchBytes} readwrite transactions; ${batchBytes} value bytes per transaction; ${totalBytes} total value bytes`,
            notes: 'Additional symmetric scaling row using unchanged stock runners, deterministic values, strict durability and content verification. Generation/open/cleanup excluded for both engines.'
        });
    }
    workloadNames = scalingSpecs.map((spec) => spec.name);
}
assert.ok(workloadNames.length > 0);
assert.ok(!values['engine-timings'] || values.diagnostic, '--engine-timings requires --diagnostic');
const samples = count('samples', 1);
const warmups = count('warmups', 0);
const timeoutMs = count('timeout-ms', 1);
function git(...args) {
    try {
        return execFileSync('git', args, {
            cwd: sourceRoot,
            encoding: 'utf8',
            stdio: ['ignore', 'pipe', 'ignore']
        }).trim();
    } catch {
        return null;
    }
}
const sourceSha = values.sha ?? git('rev-parse', 'HEAD');
assert.ok(sourceSha, '--sha is required if the source directory has no Git HEAD');
const timestamp = new Date().toISOString();
const out = resolve(
    values.out ??
        join(
            here,
            '../bench/results',
            `${values.label}-${values.diagnostic ? 'diagnostic' : 'benchmark'}-${timestamp.replaceAll(':', '-')}.json`
        )
);
const fingerprintPaths = [
    'crates/engine/src/engine.rs',
    'crates/engine/src/wal.rs',
    'crates/engine/src/btree.rs',
    'crates/engine/src/checksum.rs',
    'crates/engine/src/page.rs',
    'crates/engine/src/overflow.rs',
    'crates/engine/src/prepared_value.rs',
    'crates/engine/src/value.rs',
    'crates/engine/src/change_feed.rs',
    'crates/engine/src/pager.rs',
    'crates/engine/js/opfs_shim.js',
    'packages/sdk/src/worker.ts',
    'packages/sdk/src/worker-server.ts',
    'packages/sdk/src/worker-client.ts',
    'packages/sdk/src/worker-protocol.ts',
    'packages/sdk/bench/bench-runner.ts',
    'packages/sdk/bench/workloads.ts',
    'packages/sdk/bench/moyodb-baseline.ts',
    'packages/sdk/bench/indexeddb-baseline.ts',
    'packages/sdk/public/engine/moyodb_engine.js',
    'packages/sdk/public/engine/moyodb_engine_bg.wasm'
];
const fingerprints = {};
for (const path of fingerprintPaths) {
    try {
        const content = await readFile(join(sourceRoot, path));
        fingerprints[path] = { sha256: createHash('sha256').update(content).digest('hex'), bytes: content.length };
    } catch (error) {
        fingerprints[path] = { error: error.code ?? String(error) };
    }
}
if (engines.includes('moyodb')) {
    assert.ok(
        fingerprints['packages/sdk/public/engine/moyodb_engine_bg.wasm'].sha256,
        'Build release WASM before benchmarking.'
    );
}

// Serialized into each diagnostic Worker. Keep imports and engine initialization
// in the original Worker to preserve module load order.
function installWorkerProbe(config) {
    const self = globalThis;
    const { navigator } = self;
    const nativePost = self.postMessage.bind(self);
    const metrics = Object.create(null);
    const commits = [];
    const hookErrors = [];
    const pending = new Map();
    const wire = { requests: 0, requestMessages: 0, responses: 0, responseMessages: 0 };
    const syncTotals = {
        calls: 0,
        elapsedMs: 0,
        reads: 0,
        writes: 0,
        readBytes: 0,
        writtenBytes: 0,
        flushes: 0,
        getSizes: 0,
        truncates: 0
    };
    const engineMethods = [];
    const queuedUntilImport = [];
    let imported = false;
    const version = 1;
    const metric = (category, file, operation) => {
        const key = `${category}|${file}|${operation}`;
        return (metrics[key] ??= {
            category,
            file,
            operation,
            calls: 0,
            errors: 0,
            elapsedMs: 0,
            requestedBytes: 0,
            transferredBytes: 0,
            maxRequestBytes: 0
        });
    };
    const snapshot = () => ({ version, metrics, wire, commits, syncTotals, hookErrors, engineMethods });
    const emitSnapshot = (requestId = null) =>
        nativePost({ [config.token]: 'snapshot', requestId, snapshot: snapshot() });
    function replace(object, name, replacement) {
        try {
            const descriptor = Object.getOwnPropertyDescriptor(object, name);
            Object.defineProperty(object, name, {
                ...(descriptor ?? { configurable: true, enumerable: false }),
                writable: true,
                value: replacement
            });
            return true;
        } catch (error) {
            hookErrors.push(`${name}: ${error.name}: ${error.message}`);
            return false;
        }
    }
    function wrapHandle(handle, file) {
        for (const operation of ['read', 'write', 'flush', 'getSize', 'truncate', 'close']) {
            const native = handle[operation];
            if (typeof native !== 'function') continue;
            replace(handle, operation, function (...args) {
                const item = metric('sync', file, operation);
                const requested = operation === 'read' || operation === 'write' ? (args[0]?.byteLength ?? 0) : 0;
                item.calls += 1;
                item.requestedBytes += requested;
                item.maxRequestBytes = Math.max(item.maxRequestBytes, requested);
                syncTotals.calls += 1;
                if (operation === 'read') syncTotals.reads += 1;
                if (operation === 'write') syncTotals.writes += 1;
                if (operation === 'flush') syncTotals.flushes += 1;
                if (operation === 'getSize') syncTotals.getSizes += 1;
                if (operation === 'truncate') syncTotals.truncates += 1;
                const started = performance.now();
                try {
                    const result = native.apply(this, args);
                    if ((operation === 'read' || operation === 'write') && Number.isFinite(result)) {
                        item.transferredBytes += result;
                        if (operation === 'read') syncTotals.readBytes += result;
                        else syncTotals.writtenBytes += result;
                    }
                    return result;
                } catch (error) {
                    item.errors += 1;
                    throw error;
                } finally {
                    const elapsed = performance.now() - started;
                    item.elapsedMs += elapsed;
                    syncTotals.elapsedMs += elapsed;
                }
            });
        }
        return handle;
    }
    function wrapAsync(object, operation, fileForCall, after) {
        if (!object || typeof object[operation] !== 'function') return;
        const native = object[operation];
        replace(object, operation, async function (...args) {
            const file = fileForCall(this, args);
            const item = metric('metadata', file, operation);
            item.calls += 1;
            const started = performance.now();
            try {
                const result = await native.apply(this, args);
                return after ? after(result, this, args) : result;
            } catch (error) {
                item.errors += 1;
                throw error;
            } finally {
                item.elapsedMs += performance.now() - started;
            }
        });
    }
    try {
        wrapAsync(self.FileSystemDirectoryHandle?.prototype, 'getDirectoryHandle', (_self, args) => String(args[0]));
        wrapAsync(self.FileSystemDirectoryHandle?.prototype, 'getFileHandle', (_self, args) => String(args[0]));
        wrapAsync(self.FileSystemDirectoryHandle?.prototype, 'removeEntry', (_self, args) => String(args[0]));
        wrapAsync(self.FileSystemFileHandle?.prototype, 'getFile', (file) => file.name);
        wrapAsync(
            self.FileSystemFileHandle?.prototype,
            'createSyncAccessHandle',
            (file) => file.name,
            (handle, file) => wrapHandle(handle, file.name)
        );
        wrapAsync(Object.getPrototypeOf(navigator.storage), 'getDirectory', () => 'origin-root');
    } catch (error) {
        hookErrors.push(`installation: ${error.name}: ${error.message}`);
    }
    // Exclude diagnostic control messages from SDK protocol counts.
    self.addEventListener('message', (event) => {
        const data = event.data;
        if (data?.[config.token] === 'snapshot') {
            event.stopImmediatePropagation();
            emitSnapshot(data.requestId);
            return;
        }
        // Queue messages until the imported Worker module installs its handlers.
        if (!imported) {
            event.stopImmediatePropagation();
            queuedUntilImport.push({ data, origin: event.origin, ports: event.ports });
            return;
        }
        const requests =
            data?.type === 'moyodb:worker-protocol:request-batch'
                ? data.requests
                : data?.type === 'moyodb:worker-protocol:request'
                  ? [data]
                  : [];
        if (requests.length > 0) {
            wire.requestMessages += 1;
            wire.requests += requests.length;
            for (const request of requests) pending.set(request.id, request.command);
        }
    });
    self.postMessage = function (...args) {
        const data = args[0];
        const replies =
            data?.type === 'moyodb:worker-protocol:response-batch'
                ? data.responses
                : data?.type === 'moyodb:worker-protocol:response'
                  ? [data]
                  : [];
        let closing = false;
        if (replies.length > 0) {
            wire.responseMessages += 1;
            wire.responses += replies.length;
            for (const reply of replies) {
                const command = pending.get(reply.id);
                if (command === 'close' || command === 'destroy') closing = true;
                pending.delete(reply.id);
            }
        }
        // Send final stats before the close/destroy response lets the SDK terminate this Worker.
        if (closing) emitSnapshot();
        return nativePost(...args);
    };
    self.__moyoBrowserBenchDiagnostic = {
        timeSync(operation, fn) {
            const item = metric('sdk', 'DbWorker', operation);
            item.calls += 1;
            const start = performance.now();
            try {
                return fn();
            } catch (error) {
                item.errors += 1;
                throw error;
            } finally {
                item.elapsedMs += performance.now() - start;
            }
        },
        async timeAsync(operation, fn) {
            const item = metric('sdk', 'DbWorker', operation);
            item.calls += 1;
            const start = performance.now();
            try {
                return await fn();
            } catch (error) {
                item.errors += 1;
                throw error;
            } finally {
                item.elapsedMs += performance.now() - start;
            }
        },
        imported() {
            imported = true;
            for (const event of queuedUntilImport.splice(0)) self.dispatchEvent(new MessageEvent('message', event));
        },
        wrapWasmEngine(Engine) {
            if (!Engine?.prototype) {
                hookErrors.push('WasmEngine export absent');
                return;
            }
            for (const name of Object.getOwnPropertyNames(Engine.prototype)) {
                if (name === 'constructor' || name === '__destroy_into_raw') continue;
                const descriptor = Object.getOwnPropertyDescriptor(Engine.prototype, name);
                const native = descriptor?.value;
                if (typeof native !== 'function') continue;
                const wrapped = function (...args) {
                    const item = metric('engine', 'WasmEngine', name);
                    item.calls += 1;
                    const start = performance.now();
                    const before = name === 'commit_tx' ? { ...syncTotals } : null;
                    const finish = (failed) => {
                        const elapsedMs = performance.now() - start;
                        item.elapsedMs += elapsedMs;
                        if (failed) item.errors += 1;
                        if (before) {
                            const opfs = {};
                            for (const key of Object.keys(syncTotals)) opfs[key] = syncTotals[key] - before[key];
                            commits.push({ elapsedMs, failed, opfs });
                        }
                    };
                    let result;
                    try {
                        result = native.apply(this, args);
                    } catch (error) {
                        finish(true);
                        throw error;
                    }
                    if (result && typeof result.then === 'function') {
                        return result.then(
                            (value) => {
                                finish(false);
                                return value;
                            },
                            (error) => {
                                finish(true);
                                throw error;
                            }
                        );
                    }
                    finish(false);
                    return result;
                };
                if (replace(Engine.prototype, name, wrapped)) engineMethods.push(name);
            }
        }
    };
}

function installPageProbe({ token, workerProbeSource }) {
    const { location } = globalThis;
    const NativeWorker = globalThis.Worker;
    const workers = new Map();
    const records = [];
    let workerId = 0;
    let requestId = 0;
    class DiagnosticWorker extends NativeWorker {
        constructor(url, options) {
            const originalUrl = new URL(String(url), location.href).href;
            // SDK requests wait for READY, so module import cannot lose the open request.
            if (!new URL(originalUrl).pathname.endsWith('/src/worker.ts')) return new NativeWorker(url, options);
            if (options?.type !== 'module')
                throw new Error(`Diagnostic wrapper requires a module Worker: ${originalUrl}`);
            const bootstrap = `(${workerProbeSource})(${JSON.stringify({ token })});\nawait import(${JSON.stringify(originalUrl)});\nself.__moyoBrowserBenchDiagnostic.imported();`;
            const blobUrl = URL.createObjectURL(new Blob([bootstrap], { type: 'text/javascript' }));
            super(blobUrl, options);
            const id = ++workerId;
            const info = {
                id,
                originalUrl,
                blobUrl,
                worker: this,
                terminated: false,
                latest: null,
                pending: new Map(),
                createdAtMs: performance.now()
            };
            workers.set(id, info);
            this.addEventListener('message', (event) => {
                if (event.data?.type === 'moyodb:worker-protocol:ready')
                    info.readyElapsedMs = performance.now() - info.createdAtMs;
                if (event.data?.[token] !== 'snapshot') return;
                event.stopImmediatePropagation();
                info.latest = event.data.snapshot;
                const pending = info.pending.get(event.data.requestId);
                if (pending) {
                    clearTimeout(pending.timer);
                    info.pending.delete(event.data.requestId);
                    pending.resolve(info.latest);
                }
            });
            this.addEventListener('error', (event) => {
                info.error = event.message;
                for (const pending of info.pending.values()) {
                    clearTimeout(pending.timer);
                    pending.reject(new Error(event.message));
                }
                info.pending.clear();
            });
            this.__diagnosticWorkerId = id;
        }
        terminate() {
            const info = workers.get(this.__diagnosticWorkerId);
            info.terminated = true;
            URL.revokeObjectURL(info.blobUrl);
            for (const pending of info.pending.values()) {
                clearTimeout(pending.timer);
                pending.resolve(info.latest);
            }
            info.pending.clear();
            return super.terminate();
        }
    }
    globalThis.Worker = DiagnosticWorker;
    async function snapshots() {
        const results = [];
        for (const info of workers.values()) {
            let snapshot = info.latest;
            if (!info.terminated) {
                snapshot = await new Promise((resolve, reject) => {
                    const id = ++requestId;
                    const timer = setTimeout(() => {
                        info.pending.delete(id);
                        reject(new Error(`diagnostic Worker ${info.id} snapshot timeout`));
                    }, 15000);
                    info.pending.set(id, { resolve, reject, timer });
                    NativeWorker.prototype.postMessage.call(info.worker, { [token]: 'snapshot', requestId: id });
                });
            }
            results.push({
                id: info.id,
                originalUrl: info.originalUrl,
                terminated: info.terminated,
                error: info.error,
                readyElapsedMs: info.readyElapsedMs,
                snapshot
            });
        }
        return results;
    }
    function subtract(after, before) {
        const baseline = new Map(before.map((worker) => [worker.id, worker.snapshot]));
        const deltas = [];
        for (const worker of after) {
            if (!worker.snapshot) continue;
            const old = baseline.get(worker.id);
            const current = worker.snapshot;
            const metrics = [];
            for (const [key, item] of Object.entries(current.metrics)) {
                const previous = old?.metrics[key];
                const calls = item.calls - (previous?.calls ?? 0);
                if (calls === 0) continue;
                const delta = { ...item };
                for (const field of ['calls', 'errors', 'elapsedMs', 'requestedBytes', 'transferredBytes'])
                    delta[field] -= previous?.[field] ?? 0;
                // Keep the lifetime maximum per Worker, file and operation; do not subtract it.
                delta.maxRequestBytesScope = 'worker-lifetime';
                metrics.push(delta);
            }
            const wire = {};
            for (const key of Object.keys(current.wire)) wire[key] = current.wire[key] - (old?.wire[key] ?? 0);
            const commits = current.commits.slice(old?.commits.length ?? 0);
            if (metrics.length || Object.values(wire).some(Boolean) || commits.length) {
                deltas.push({
                    id: worker.id,
                    originalUrl: worker.originalUrl,
                    metrics,
                    wire,
                    commits,
                    hookErrors: current.hookErrors,
                    engineMethods: current.engineMethods
                });
            }
        }
        return deltas;
    }
    async function measure(ctx, phase, operation) {
        const before = await snapshots();
        const started = performance.now();
        let error;
        try {
            return await operation();
        } catch (caught) {
            error = `${caught.name}: ${caught.message}`;
            throw caught;
        } finally {
            const elapsedMs = performance.now() - started;
            const after = await snapshots();
            records.push({
                engine: ctx.engine,
                workload: ctx.workload.name,
                dbName: ctx.dbName,
                sampleIndex: ctx.sampleIndex,
                phase,
                elapsedMs,
                error,
                workers: subtract(after, before)
            });
        }
    }
    globalThis.__moyoPageDiagnostic = {
        records,
        snapshots,
        async wrapRunners() {
            const { moyoDbBaseline } = await import(
                new URL('/bench/moyodb-baseline.ts', globalThis.location.href).href
            );
            const prepare = moyoDbBaseline.prepare;
            moyoDbBaseline.prepare = async (ctx) => {
                const cleanup = await measure(ctx, 'prepare', () => prepare(ctx));
                return cleanup ? () => measure(ctx, 'cleanup', cleanup) : undefined;
            };
            for (const phase of ['run', 'verify']) {
                const original = moyoDbBaseline[phase];
                moyoDbBaseline[phase] = (ctx) => measure(ctx, phase, () => original(ctx));
            }
        }
    };
}

// Workload samples exclude this post-suite probe's writes, byte verification,
// close and reopen measurements.
async function reopenProbe({ engines, prefix }) {
    const { indexedDB } = globalThis;
    const { openDB } = await import(new URL('/src/index.ts', globalThis.location.href).href);
    const { keyBytes, valueBytes } = await import(new URL('/bench/workloads.ts', globalThis.location.href).href);
    const count = 128,
        valueSize = 4096;
    const entries = Array.from({ length: count }, (_, index) => [keyBytes(index, 16), valueBytes(index, valueSize)]);
    const results = [];
    function verify(rows) {
        if (rows.length !== count) throw new Error('reopen probe record count mismatch');
        for (let i = 0; i < count; i += 1) {
            const value = rows[i];
            if (!(value instanceof Uint8Array) || value.length !== valueSize)
                throw new Error(`reopen probe missing value ${i}`);
            for (let j = 0; j < valueSize; j += 1)
                if (value[j] !== entries[i][1][j]) throw new Error(`reopen probe byte mismatch ${i}:${j}`);
        }
    }
    function idbOpen(name, create) {
        return new Promise((resolve, reject) => {
            const request = indexedDB.open(name, 1);
            request.onupgradeneeded = () => {
                if (create) request.result.createObjectStore('kv');
                else request.transaction.abort();
            };
            request.onsuccess = () => resolve(request.result);
            request.onerror = () => reject(request.error);
        });
    }
    function idbDone(tx) {
        return new Promise((resolve, reject) => {
            tx.oncomplete = resolve;
            tx.onabort = tx.onerror = () => reject(tx.error);
        });
    }
    async function idbRead(db) {
        const tx = db.transaction('kv', 'readonly');
        const done = idbDone(tx);
        const requests = entries.map(([key]) => tx.objectStore('kv').get(key));
        await done;
        return requests.map((request) => request.result);
    }
    for (const engine of engines) {
        const name = `${prefix}-${engine}`;
        if (engine === 'moyodb') {
            let db = await openDB(name, { requestPersistence: false });
            try {
                await db.createStore('kv');
                const tx = await db.begin('readwrite');
                await tx.putMany('kv', entries);
                await tx.commit();
                verify(
                    await db.getMany(
                        'kv',
                        entries.map(([key]) => key)
                    )
                );
                let started = performance.now();
                await db.close();
                const closeMs = performance.now() - started;
                started = performance.now();
                db = await openDB(name, { requestPersistence: false, createIfMissing: false });
                const reopenMs = performance.now() - started;
                verify(
                    await db.getMany(
                        'kv',
                        entries.map(([key]) => key)
                    )
                );
                results.push({
                    engine,
                    count,
                    valueSize,
                    closeMs,
                    reopenMs,
                    verifiedBeforeClose: true,
                    verifiedAfterReopen: true,
                    guarantee: 'clean API close then reopen; not a power-loss or crash test'
                });
            } finally {
                await db.destroy();
            }
        } else {
            let db = await idbOpen(name, true);
            try {
                const tx = db.transaction('kv', 'readwrite', { durability: 'strict' });
                if (tx.durability !== 'strict') throw new Error('IndexedDB did not report strict durability');
                const done = idbDone(tx);
                for (const [key, value] of entries) tx.objectStore('kv').put(value, key);
                await done;
                verify(await idbRead(db));
                let started = performance.now();
                db.close();
                const closeMs = performance.now() - started;
                started = performance.now();
                db = await idbOpen(name, false);
                const reopenMs = performance.now() - started;
                verify(await idbRead(db));
                results.push({
                    engine,
                    count,
                    valueSize,
                    closeMs,
                    reopenMs,
                    verifiedBeforeClose: true,
                    verifiedAfterReopen: true,
                    guarantee:
                        'clean API close then reopen; IDB closeMs is synchronous API return, not completed physical teardown'
                });
            } finally {
                db.close();
                await new Promise((resolve, reject) => {
                    const request = indexedDB.deleteDatabase(name);
                    request.onsuccess = resolve;
                    request.onerror = () => reject(request.error);
                });
            }
        }
    }
    return results;
}

function contentParity(report) {
    return workloadNames.map((name) => {
        const rows = report.results.filter((row) => row.workloadName === name);
        if (rows.length < 2) return { workload: name, status: 'single-engine' };
        if (rows.some((row) => row.status !== 'ok')) return { workload: name, status: 'incomplete' };
        for (const field of [
            'recordCount',
            'keySize',
            'valueSize',
            'batchSize',
            'transactionBoundaries',
            'sampleCount',
            'warmupCount'
        ]) {
            assert.equal(rows[0][field], rows[1][field], `${name}: ${field} parity`);
        }
        assert.deepEqual(rows[0].contentChecksums, rows[1].contentChecksums, `${name}: content parity`);
        return {
            workload: name,
            status: rows[0].contentChecksums.every((checksum) => checksum === null) ? 'no-content' : 'matched',
            checksums: rows[0].contentChecksums
        };
    });
}

const require = createRequire(join(resolve(values['dependency-root']), 'package.json'));
const { chromium } = require('@playwright/test');
let vite;
if (values.serve) {
    const { createServer } = await import(pathToFileURL(require.resolve('vite')).href);
    const target = new URL(values.url);
    vite = await createServer({
        root: join(sourceRoot, 'packages/sdk'),
        configFile: join(sourceRoot, 'packages/sdk/vite.config.ts'),
        server: { host: target.hostname, port: Number(target.port || 80), strictPort: true }
    });
    await vite.listen();
}
const profile = await mkdtemp(join(tmpdir(), 'moyo-browser-bench-'));
const launchArgs = [];
const executablePath = process.env.MOYO_CHROMIUM_EXECUTABLE || process.env.MOYODB_CHROMIUM_EXECUTABLE_PATH || undefined;
let context;
let output;
try {
    context = await chromium.launchPersistentContext(profile, {
        headless: true,
        executablePath,
        args: launchArgs,
        timeout: 60000
    });
    if (values.diagnostic) {
        const token = `moyo-diagnostic-${randomBytes(12).toString('hex')}`;
        await context.addInitScript(installPageProbe, { token, workerProbeSource: installWorkerProbe.toString() });
        if (values['engine-timings']) {
            await context.route('**/engine/moyodb_engine.js', async (route) => {
                const response = await route.fetch();
                const source = await response.text();
                const suffix = '\n;globalThis.__moyoBrowserBenchDiagnostic?.wrapWasmEngine(WasmEngine);\n';
                await route.fulfill({ response, body: source + suffix });
            });
            await context.route('**/src/worker.ts*', async (route) => {
                const response = await route.fetch();
                let source = await response.text();
                // Preserve await order and evaluate each expression once.
                source = source.replaceAll(
                    'await assertCapabilities()',
                    "await globalThis.__moyoBrowserBenchDiagnostic.timeAsync('assertCapabilities', () => assertCapabilities())"
                );
                // Keep synchronous capability checks synchronous.
                source = source.replaceAll(
                    'assertCapabilities();',
                    "globalThis.__moyoBrowserBenchDiagnostic.timeSync('assertCapabilities', () => assertCapabilities());"
                );
                source = source.replaceAll(
                    'await this.lease.acquire(request.dbName, request.options.ownerWaitMs)',
                    "await globalThis.__moyoBrowserBenchDiagnostic.timeAsync('lease.acquire', () => this.lease.acquire(request.dbName, request.options.ownerWaitMs))"
                );
                source = source.replaceAll(
                    'await this.loadWasm()',
                    "await globalThis.__moyoBrowserBenchDiagnostic.timeAsync('loadWasm', () => this.loadWasm())"
                );
                await route.fulfill({ response, body: source });
            });
        }
    }
    const page = context.pages()[0] ?? (await context.newPage());
    const browserErrors = [];
    page.on('console', (message) => {
        if (message.text().startsWith('[bench]')) console.error(message.text());
    });
    page.on('pageerror', (error) => browserErrors.push(error.stack ?? String(error)));
    const response = await page.goto(values.url, { waitUntil: 'networkidle', timeout: 60000 });
    assert.ok(response?.ok(), `benchmark page returned HTTP ${response?.status()}`);
    await page.waitForFunction(() => typeof globalThis.moyodbBench?.runBenchmarkSuite === 'function');
    const cdp = await context.newCDPSession(page);
    const browserVersion = await cdp.send('Browser.getVersion');
    await cdp.detach();
    const unknownNames = await page.evaluate(
        async ({ names, scalingSpecs }) => {
            const { WORKLOADS } = await import(new URL('/bench/workloads.ts', globalThis.location.href).href);
            const template = WORKLOADS.find((workload) => workload.name === 'large_value_64kb');
            for (const spec of scalingSpecs) {
                if (WORKLOADS.some((workload) => workload.name === spec.name))
                    throw new Error(`Duplicate scaling workload ${spec.name}`);
                WORKLOADS.push({ ...template, ...spec, smoke: false, tags: ['write', 'scaling'] });
            }
            return names.filter((name) => !WORKLOADS.some((workload) => workload.name === name));
        },
        { names: workloadNames, scalingSpecs }
    );
    assert.deepEqual(unknownNames, [], 'Unknown workload names');
    if (values.diagnostic) await page.evaluate(() => globalThis.__moyoPageDiagnostic.wrapRunners());
    const report = await page.evaluate((options) => globalThis.moyodbBench.runBenchmarkSuite(options), {
        engines,
        profile: 'full',
        workloadNames,
        sampleCountOverride: samples,
        warmupCountOverride: warmups,
        dbNamePrefix: `gap-${values.label}-${Date.now()}`,
        workloadTimeoutMs: timeoutMs,
        persistentContext: true,
        gitSha: sourceSha,
        indexedDbDurability: 'strict'
    });
    output = {
        ...report,
        measurement: {
            kind: values.diagnostic ? 'instrumented-browser-diagnostic' : 'full-browser-comparison',
            label: values.label,
            sourceRoot,
            sourceSha,
            localHead: git('rev-parse', 'HEAD'),
            injectedScalingSpecs: scalingSpecs,
            trackedDiff: git('diff', '--stat', 'HEAD'),
            fingerprints,
            runnerSha256: createHash('sha256')
                .update(await readFile(fileURLToPath(import.meta.url)))
                .digest('hex'),
            browserVersion,
            node: process.version,
            platform: `${process.platform}/${process.arch}`,
            executablePath: executablePath ?? 'Playwright-managed Chromium',
            launchArgs,
            persistentContext: true,
            freshProfile: true,
            profileDirectory: values['keep-profile'] ? profile : '(removed after run)',
            cacheQualification:
                'Stock suite detects WASM build profile before samples; fresh empty DB open is not cold browser/network/module startup.',
            durabilityQualification:
                'IndexedDB strict transactions; current Moyo engine commits flush the WAL and use bounded checkpointing. Stock environment string may predate that implementation.',
            timingQualification: values.diagnostic
                ? 'Instrumented diagnostic only. Raw stock elapsed also contains profiling snapshot requests. diagnostics.records[].elapsedMs excludes snapshot requests but includes installed hook overhead.'
                : 'Unmodified stock timed regions and content checks, in a real persistent browser context.'
        },
        browserErrors
    };
    if (values.diagnostic) {
        output.diagnostics = await page.evaluate(async () => ({
            records: globalThis.__moyoPageDiagnostic.records,
            finalWorkers: await globalThis.__moyoPageDiagnostic.snapshots()
        }));
    }
    output.contentParity = contentParity(report);
    if (values['reopen-check'])
        output.postSuiteReopenProbe = await page.evaluate(reopenProbe, { engines, prefix: `gap-reopen-${Date.now()}` });
    await mkdir(dirname(out), { recursive: true });
    await writeFile(out, JSON.stringify(output, null, 2) + '\n');
    console.log(out);
    assert.ok(
        !report.results.some((row) => row.status !== 'ok'),
        'A selected applicable workload failed or was skipped; inspect saved raw JSON.'
    );
    if (engines.includes('moyodb'))
        assert.equal(report.environment.wasmBuildMode, 'release', 'Release WASM is required.');
    assert.deepEqual(browserErrors, [], 'Page errors occurred; inspect saved raw JSON.');
    if (values.diagnostic) {
        const allHookErrors = output.diagnostics.finalWorkers.flatMap((worker) => worker.snapshot?.hookErrors ?? []);
        assert.deepEqual(allHookErrors, [], 'Diagnostic hook failed; inspect saved raw JSON.');
    }
} catch (error) {
    if (output) {
        output.runnerError = error.stack ?? String(error);
        await mkdir(dirname(out), { recursive: true });
        await writeFile(out, JSON.stringify(output, null, 2) + '\n');
    }
    throw error;
} finally {
    await context?.close();
    await vite?.close();
    if (!values['keep-profile']) await rm(profile, { recursive: true, force: true });
}
