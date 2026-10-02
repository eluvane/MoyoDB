import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { performance } from 'node:perf_hooks';
import { parseArgs } from 'node:util';
import { pathToFileURL } from 'node:url';
import { isMainThread, parentPort, Worker, workerData } from 'node:worker_threads';

// A transport component benchmark, NOT a database/browser/IndexedDB benchmark.
// Both variants use their real SDK client, protocol and server in a real Node
// Worker. The engine is an identical preloaded in-memory fixture, without WASM,
// OPFS, persistence or B-tree work. Compilation, Worker startup, preload and
// byte-for-byte verification are outside the timed region in BOTH variants.
const modules = ['worker-client', 'worker-server', 'worker-protocol', 'internal'];

async function compile(source, destination) {
    const { default: ts } = await import('typescript');
    await mkdir(destination);
    const hashes = {};
    for (const name of modules) {
        const input = await readFile(join(source, `${name}.ts`), 'utf8');
        hashes[name] = createHash('sha256').update(input).digest('hex');
        const result = ts.transpileModule(input, {
            fileName: `${name}.ts`,
            reportDiagnostics: true,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
        });
        assert.ok(!(result.diagnostics ?? []).some((item) => item.category === ts.DiagnosticCategory.Error));
        await writeFile(join(destination, `${name}.mjs`), result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'"));
    }
    return { hashes, typescript: ts.version };
}

async function serve() {
    const { exposeWorkerApi } = await import(workerData.serverUrl);
    const scope = new EventTarget();
    scope.location = { origin: 'https://moyo-transport-bench.test' };
    let values = [];
    let active = false;
    let counts = {};
    scope.postMessage = (message, transfer = []) => {
        const replies = message.responses ?? [message];
        const binary = replies.filter((reply) => reply.result instanceof Uint8Array);
        if (binary.length > 0) {
            counts.responseMessages += 1;
            counts.responseBytes += binary.reduce((sum, reply) => sum + reply.result.byteLength, 0);
        }
        parentPort.postMessage(message, transfer);
    };
    parentPort.on('message', (data) => scope.dispatchEvent(new MessageEvent('message', { data })));
    const server = exposeWorkerApi({
        open: async ({ count, valueBytes }) => {
            active = false;
            counts = { gets: 0, begins: 0, rollbacks: 0, responseMessages: 0, responseBytes: 0 };
            values = Array.from({ length: count }, (_, index) => {
                const value = new Uint8Array(valueBytes).fill((index * 17 + 1) & 255);
                new DataView(value.buffer).setUint32(valueBytes - 4, index, true);
                return value;
            });
        },
        begin: async (mode) => {
            assert.equal(mode, 'readonly');
            assert.equal(active, false);
            active = true;
            counts.begins += 1;
            return 1;
        },
        get: async (txId, store, key) => {
            assert.ok(active);
            assert.equal(txId, 1);
            assert.equal(store, 'kv');
            counts.gets += 1;
            const index = new DataView(key.buffer, key.byteOffset).getUint32(0, true);
            return values[index].slice();
        },
        rollback: async (txId) => {
            assert.equal(txId, 1);
            assert.ok(active);
            counts.rollbacks += 1;
            active = false;
        },
        stats: async () => ({ ...counts })
    }, scope);
    parentPort.once('close', () => server.close());
}

async function connect(destination) {
    const { WorkerProtocolClient } = await import(pathToFileURL(join(destination, 'worker-client.mjs')).href);
    const worker = new Worker(new URL(import.meta.url), {
        workerData: { serverUrl: pathToFileURL(join(destination, 'worker-server.mjs')).href }
    });
    const endpoint = new EventTarget();
    let requests = 0;
    endpoint.postMessage = (message, transfer = []) => {
        requests += 1;
        worker.postMessage(message, transfer);
    };
    worker.on('message', (data) => endpoint.dispatchEvent(new MessageEvent('message', { data })));
    worker.on('error', (error) => {
        endpoint.dispatchEvent(Object.assign(new Event('error'), { error, message: error.message }));
    });
    const client = new WorkerProtocolClient(endpoint);
    worker.on('exit', (code) => client.dispose(new Error(`benchmark Worker exited (${code})`)));
    try {
        await deadline(client.whenReady());
        return {
            client,
            get requestMessages() { return requests; },
            async close() {
                client.dispose();
                await worker.terminate();
            }
        };
    } catch (error) {
        client.dispose();
        await worker.terminate();
        throw error;
    }
}

async function deadline(promise) {
    let timer;
    try {
        return await Promise.race([
            promise,
            new Promise((_, reject) => {
                timer = setTimeout(() => reject(new Error('transport benchmark stalled')), 30000);
            })
        ]);
    } finally {
        clearTimeout(timer);
    }
}

async function sample(connection, spec) {
    const { client } = connection;
    await client.open(spec);
    const keys = Array.from({ length: spec.count }, (_, index) => {
        const key = new Uint8Array(16);
        new DataView(key.buffer).setUint32(0, (index * 7919) % spec.count, true);
        return key;
    });
    let values;
    const requestStart = connection.requestMessages;
    const start = performance.now();
    const txId = await client.begin('readonly');
    try {
        if (spec.mode === 'pipelined') {
            values = await Promise.all(keys.map((key) => client.get(txId, 'kv', key)));
        } else {
            values = [];
            for (const key of keys) values.push(await client.get(txId, 'kv', key));
        }
    } finally {
        await client.rollback(txId);
    }
    const elapsedMs = performance.now() - start;
    // Exclude the two unchanged begin/rollback messages from get-only counts.
    const requestMessages = connection.requestMessages - requestStart - 2;
    const counts = await client.stats();
    assert.equal(counts.gets, spec.count);
    assert.equal(counts.begins, 1);
    assert.equal(counts.rollbacks, 1);
    assert.equal(counts.responseBytes, spec.count * spec.valueBytes);
    for (let index = 0; index < keys.length; index += 1) {
        assert.equal(keys[index].byteLength, 16);
        const expectedIndex = (index * 7919) % spec.count;
        const value = values[index];
        assert.equal(value.byteLength, spec.valueBytes);
        for (let offset = 0; offset < spec.valueBytes - 4; offset += 1) {
            assert.equal(value[offset], (expectedIndex * 17 + 1) & 255);
        }
        assert.equal(new DataView(value.buffer, value.byteOffset).getUint32(spec.valueBytes - 4, true), expectedIndex);
    }
    return { elapsedMs, requestMessages, ...counts };
}

function summarize(samples) {
    const times = samples.map((item) => item.elapsedMs).sort((left, right) => left - right);
    const first = samples[0];
    for (const item of samples) {
        for (const key of ['requestMessages', 'responseMessages', 'responseBytes', 'gets', 'begins', 'rollbacks']) {
            assert.equal(item[key], first[key], `${key} changed between samples`);
        }
    }
    return {
        ...first,
        elapsedMs: undefined,
        medianMs: times[Math.floor(times.length / 2)],
        p95Ms: times[Math.ceil(times.length * 0.95) - 1],
        samplesMs: samples.map((item) => item.elapsedMs)
    };
}

async function main() {
    const { values, positionals } = parseArgs({
        allowPositionals: true,
        options: { samples: { type: 'string', default: '11' }, warmup: { type: 'string', default: '3' } }
    });
    if (positionals.length !== 2) {
        throw new Error('usage: node bench-response-batching.mjs BEFORE_SRC AFTER_SRC [--samples 11] [--warmup 3]');
    }
    const samples = Number(values.samples);
    const warmup = Number(values.warmup);
    assert.ok(Number.isSafeInteger(samples) && samples > 0 && samples <= 1000);
    assert.ok(Number.isSafeInteger(warmup) && warmup >= 0 && warmup <= 1000);
    const temporary = await mkdtemp(join(tmpdir(), 'moyo-response-bench-'));
    const connections = {};
    try {
        const metadata = {};
        for (const [index, label] of ['before', 'after'].entries()) {
            const source = resolve(positionals[index]);
            const destination = join(temporary, label);
            metadata[label] = { source, ...(await compile(source, destination)) };
            connections[label] = await connect(destination);
        }
        const cases = [
            { mode: 'pipelined', count: 2, valueBytes: 256 },
            { mode: 'pipelined', count: 128, valueBytes: 256 },
            { mode: 'pipelined', count: 1000, valueBytes: 256 },
            { mode: 'pipelined', count: 10000, valueBytes: 256 },
            { mode: 'pipelined', count: 128, valueBytes: 64 * 1024 },
            { mode: 'sequential', count: 1000, valueBytes: 256 }
        ];
        const results = [];
        for (const spec of cases) {
            const measured = { before: [], after: [] };
            for (let index = -warmup; index < samples; index += 1) {
                const order = Math.abs(index) % 2 === 0 ? ['before', 'after'] : ['after', 'before'];
                for (const label of order) {
                    const result = await deadline(sample(connections[label], spec));
                    if (index >= 0) measured[label].push(result);
                }
            }
            const before = summarize(measured.before);
            const after = summarize(measured.after);
            results.push({ ...spec, before, after, speedup: before.medianMs / after.medianMs });
        }
        console.log(JSON.stringify({
            level: 'Node Worker transport component; no database, WASM, OPFS or IndexedDB',
            node: process.version,
            platform: `${process.platform}/${process.arch}`,
            samples,
            warmup,
            metadata,
            results
        }, null, 2));
    } finally {
        await Promise.all(Object.values(connections).map((connection) => connection.close()));
        await rm(temporary, { recursive: true, force: true });
    }
}

if (isMainThread) await main();
else await serve();
