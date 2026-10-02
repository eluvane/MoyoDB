import assert from 'node:assert/strict';

const ORIGIN = 'https://moyo-transport.test';
const REQUEST_BATCH = 'moyodb:worker-protocol:request-batch';
const RESPONSE_BATCH = 'moyodb:worker-protocol:response-batch';

function deferred() {
    const state = {};
    state.promise = new Promise((resolve, reject) => {
        state.resolve = resolve;
        state.reject = reject;
    });
    return state;
}

async function drain() {
    // Fixed microtask turns, without timers or elapsed-time measurements.
    for (let index = 0; index < 40; index += 1) await Promise.resolve();
}

class Surface {
    listeners = new Map();
    errors = [];
    addEventListener(type, handler) {
        let handlers = this.listeners.get(type);
        if (!handlers) this.listeners.set(type, (handlers = new Set()));
        handlers.add(handler);
    }
    removeEventListener(type, handler) {
        const handlers = this.listeners.get(type);
        handlers?.delete(handler);
        if (handlers?.size === 0) this.listeners.delete(type);
    }
    emit(type, data, extra = {}) {
        for (const handler of Array.from(this.listeners.get(type) ?? [])) {
            try {
                handler({ data, origin: '', ...extra });
            } catch (error) {
                this.errors.push(error);
            }
        }
    }
}

function requests(messages) {
    return messages.flatMap(({ data }) => (Array.isArray(data.requests) ? data.requests : [data]));
}

function responses(messages) {
    return messages.flatMap(({ data }) => (Array.isArray(data.responses) ? data.responses : [data]));
}

function snapshot(message, transfer) {
    const transferredBytes = transfer.reduce((total, buffer) => total + (buffer.byteLength ?? 0), 0);
    const data = structuredClone(message, { transfer });
    return { data, transferredBytes, detached: transfer.map((buffer) => buffer.byteLength) };
}

export function createTransportSuite({ WorkerProtocolClient, exposeWorkerApi, protocol, __transportWorkCounter }) {
    const tests = [];
    const observations = {};
    const test = (name, run, kind = 'semantics') => tests.push({ name, run, kind });

    async function harness(api = {}, options = {}) {
        const worker = new Surface();
        const scope = new Surface();
        scope.location = { origin: ORIGIN };
        const toWorker = [];
        const toClient = [];
        let heldReady = null;
        let requestPostError = null;
        let transferFailures = options.failResponseTransfer ? 1 : 0;
        let failedTransferAttempts = 0;
        worker.postMessage = (message, transfer = []) => {
            if (requestPostError) throw requestPostError;
            const captured = snapshot(message, transfer);
            toWorker.push(captured);
            queueMicrotask(() => scope.emit('message', captured.data));
        };
        scope.postMessage = (message, transfer = []) => {
            if (transfer.length > 0 && transferFailures > 0) {
                transferFailures -= 1;
                failedTransferAttempts += 1;
                throw new Error('injected response transfer failure');
            }
            const captured = snapshot(message, transfer);
            if (captured.data.type === protocol.WORKER_PROTOCOL_READY) {
                if (options.legacyReady) delete captured.data.batching;
                if (options.holdReady) {
                    heldReady = captured;
                    return;
                }
            }
            toClient.push(captured);
            queueMicrotask(() => worker.emit('message', captured.data));
        };
        const client = new WorkerProtocolClient(worker, { requestTimeoutMs: options.requestTimeoutMs ?? 0 });
        const server = exposeWorkerApi(api, scope);
        if (!options.holdReady) await client.whenReady();
        return {
            client,
            worker,
            scope,
            toWorker,
            toClient,
            get failedTransferAttempts() {
                return failedTransferAttempts;
            },
            releaseReady() {
                assert.ok(heldReady);
                const ready = heldReady;
                heldReady = null;
                toClient.push(ready);
                queueMicrotask(() => worker.emit('message', ready.data));
            },
            failRequests(error) {
                requestPostError = error;
            },
            injectResponse(data) {
                worker.emit('message', structuredClone(data));
            },
            dispose() {
                client.dispose();
                server.close();
            }
        };
    }

    test('single and autocommit replies preserve their own results and errors', async () => {
        const operations = [];
        let nextTx = 1;
        const h = await harness({
            begin: async (mode) => {
                operations.push(['begin', mode]);
                return nextTx++;
            },
            commit: async (id) => operations.push(['commit', id]),
            rollback: async (id) => operations.push(['rollback', id]),
            get: async (_id, _store, key) => key.slice(),
            put: async (_id, _store, _key, _value, options) => {
                operations.push(['put', options.ttl]);
                throw Object.assign(new Error('failed put'), { name: 'InjectedPutError', code: 'put-failed' });
            }
        });
        try {
            assert.deepEqual(await h.client.get(7, 'kv', new Uint8Array([3, 9])), new Uint8Array([3, 9]));
            assert.deepEqual(
                await h.client.autocommit('readonly', 'get', ['kv', new Uint8Array([4])]),
                new Uint8Array([4])
            );
            await assert.rejects(
                h.client.autocommit('readwrite', 'put', ['kv', new Uint8Array([1]), new Uint8Array([2]), { ttl: 17 }]),
                (error) => error.name === 'InjectedPutError' && error.code === 'put-failed'
            );
            assert.deepEqual(operations, [
                ['begin', 'readonly'],
                ['rollback', 1],
                ['begin', 'readwrite'],
                ['put', 17],
                ['rollback', 2]
            ]);
            assert.equal(responses(h.toClient).filter((message) => message.id !== undefined).length, 3);
        } finally {
            h.dispose();
        }
    });

    test('single partial views arrive intact and caller buffers remain reusable', async () => {
        const received = [];
        const h = await harness({
            get: async (_id, _store, key) => key.slice(),
            has: async (_id, _store, key) => key[0] === 2,
            delete: async (_id, _store, key) => key[0] === 2,
            getByIndex: async (_id, _store, _index, key) => key.slice(),
            put: async (_id, _store, key, value, options) =>
                received.push([Array.from(key), Array.from(value), options.ttl])
        });
        const backing = new Uint8Array([1, 2, 3, 4, 5, 6]);
        const key = backing.subarray(1, 3);
        const value = backing.subarray(2, 5);
        try {
            const results = await Promise.all([
                h.client.get(1, 'kv', key),
                h.client.has(2, 'kv', key),
                h.client.delete(3, 'kv', key),
                h.client.getByIndex(4, 'kv', 'by_key', key),
                h.client.put(5, 'kv', key, value, { ttl: 19 })
            ]);
            assert.deepEqual(Array.from(results[0]), [2, 3]);
            assert.equal(results[1], true);
            assert.equal(results[2], true);
            assert.deepEqual(Array.from(results[3]), [2, 3]);
            assert.deepEqual(received, [[[2, 3], [3, 4, 5], 19]]);
            assert.deepEqual(Array.from(backing), [1, 2, 3, 4, 5, 6]);
            assert.deepEqual(Array.from(await h.client.get(6, 'kv', key)), [2, 3]);
        } finally {
            h.dispose();
        }
    });

    test('bulk transfer preserves input buffers, duplicate keys and put/delete order', async () => {
        const received = [];
        const h = await harness({
            putMany: async (_id, _store, entries) =>
                received.push(entries.map(([key, value]) => [Array.from(key), Array.from(value)])),
            deleteMany: async (_id, _store, keys) => received.push(keys.map((key) => Array.from(key))),
            applyBatch: async (_id, _store, ops) =>
                received.push(ops.map((op) => [op.kind, Array.from(op.key), op.value ? Array.from(op.value) : null]))
        });
        const input = new Uint8Array([9, 1, 2, 3, 8]);
        const key = input.subarray(1, 3);
        const value = input.subarray(2, 4);
        try {
            await Promise.all([
                h.client.putMany(1, 'kv', [
                    [key, value],
                    [key, value]
                ]),
                h.client.deleteMany(2, 'kv', [key, key]),
                h.client.applyBatch(3, 'kv', [
                    { kind: 'put', key, value },
                    { kind: 'delete', key }
                ])
            ]);
            assert.deepEqual(received, [
                [
                    [
                        [1, 2],
                        [2, 3]
                    ],
                    [
                        [1, 2],
                        [2, 3]
                    ]
                ],
                [
                    [1, 2],
                    [1, 2]
                ],
                [
                    ['put', [1, 2], [2, 3]],
                    ['delete', [1, 2], null]
                ]
            ]);
            assert.deepEqual(Array.from(input), [9, 1, 2, 3, 8]);
            assert.ok(h.toWorker.every((message) => message.detached.every((length) => length === 0)));
        } finally {
            h.dispose();
        }
    });

    test('request snapshots retain the existing ready-microtask observation point', async () => {
        const seen = [];
        const h = await harness({
            put: async (_id, _store, _key, value, options) => seen.push([value[0], options.ttl]),
            putMany: async (_id, _store, entries) => seen.push([entries[0][1][0]])
        });
        try {
            for (const partial of [false, true]) {
                const backing = new Uint8Array([1, 1, 1]);
                const value = partial ? backing.subarray(1, 2) : backing;
                const options = { ttl: 1 };
                const request = h.client.put(1, 'kv', new Uint8Array([1]), value, options);
                value[0] = 2;
                options.ttl = 2;
                await Promise.resolve();
                value[0] = 3;
                options.ttl = 3;
                await request;
            }
            const value = new Uint8Array([1]);
            const request = h.client.putMany(1, 'kv', [[new Uint8Array([1]), value]]);
            value[0] = 2;
            await Promise.resolve();
            value[0] = 3;
            await request;
            assert.deepEqual(seen, [[2, 2], [2, 2], [2]]);
        } finally {
            h.dispose();
        }
    });

    test('requests wait for READY and preserve shared buffers', async () => {
        const received = [];
        const h = await harness({ put: async (_id, _store, _key, value) => received.push(value) }, { holdReady: true });
        const backing = new SharedArrayBuffer(8);
        const view = new Uint8Array(backing, 2, 2);
        view.set([1, 2]);
        try {
            const request = h.client.put(1, 'kv', new Uint8Array([1]), view);
            await drain();
            assert.equal(h.toWorker.length, 0);
            view[0] = 3;
            h.releaseReady();
            await request;
            assert.ok(received[0].buffer instanceof SharedArrayBuffer);
            assert.deepEqual(Array.from(received[0]), [3, 2]);
            view[0] = 4;
            assert.equal(received[0][0], 4);
        } finally {
            h.dispose();
        }
    });

    test('one transaction remains ordered while a different lane proceeds', async () => {
        const gate = deferred();
        const log = [];
        const h = await harness({
            put: async (id, _store, key) => {
                log.push(`start:${id}:${key[0]}`);
                if (key[0] === 1) await gate.promise;
                if (key[0] === 3) throw new Error('ordered failure');
                log.push(`end:${id}:${key[0]}`);
            },
            get: async (id) => {
                log.push(`get:${id}`);
                return null;
            },
            commit: async (id) => log.push(`commit:${id}`)
        });
        try {
            const first = h.client.put(7, 'kv', new Uint8Array([1]), new Uint8Array([9]));
            const second = h.client.put(7, 'kv', new Uint8Array([2]), new Uint8Array([9]));
            const failed = h.client.put(7, 'kv', new Uint8Array([3]), new Uint8Array([9]));
            const recovered = h.client.get(7, 'kv', new Uint8Array([4]));
            const commit = h.client.commit(7);
            const other = h.client.get(8, 'kv', new Uint8Array([1]));
            const outcomes = Promise.allSettled([first, second, failed, recovered, commit, other]);
            await drain();
            assert.deepEqual(log, ['start:7:1', 'get:8']);
            gate.resolve();
            const results = await outcomes;
            assert.equal(results[2].status, 'rejected');
            assert.ok(results.filter((_item, index) => index !== 2).every((item) => item.status === 'fulfilled'));
            assert.deepEqual(log, [
                'start:7:1',
                'get:8',
                'end:7:1',
                'start:7:2',
                'end:7:2',
                'start:7:3',
                'get:7',
                'commit:7'
            ]);
        } finally {
            gate.resolve();
            h.dispose();
        }
    });

    test('writer autocommits serialize complete begin/operation/commit lifetimes', async () => {
        const gate = deferred();
        const log = [];
        let activeWriter = 0;
        let next = 0;
        const h = await harness({
            begin: async () => {
                assert.equal(activeWriter, 0);
                const id = ++next;
                activeWriter = id;
                log.push(`begin:${id}`);
                return id;
            },
            put: async (id, _store, _key, _value, options) => {
                log.push(`put:${id}:${options.ttl}`);
                if (id === 1) await gate.promise;
            },
            commit: async (id) => {
                log.push(`commit:${id}`);
                activeWriter = 0;
            }
        });
        try {
            const pending = [1, 2].map((ttl) =>
                h.client.autocommit('readwrite', 'put', ['kv', new Uint8Array([ttl]), new Uint8Array([ttl]), { ttl }])
            );
            await drain();
            assert.deepEqual(log, ['begin:1', 'put:1:1']);
            gate.resolve();
            await Promise.all(pending);
            assert.deepEqual(log, ['begin:1', 'put:1:1', 'commit:1', 'begin:2', 'put:2:2', 'commit:2']);
        } finally {
            gate.resolve();
            h.dispose();
        }
    });

    test('exclusive commands wait for prior work and hold later work; storage bypasses them', async () => {
        const firstGate = deferred();
        const closeGate = deferred();
        const storageGate = deferred();
        const log = [];
        const h = await harness({
            get: async (id) => {
                log.push(`get:${id}`);
                if (id === 1) await firstGate.promise;
                return null;
            },
            close: async () => {
                log.push('close');
                await closeGate.promise;
            },
            storageInfo: async () => {
                log.push('storage');
                return storageGate.promise;
            }
        });
        try {
            const first = h.client.get(1, 'kv', new Uint8Array([1]));
            const close = h.client.close();
            const later = h.client.get(2, 'kv', new Uint8Array([2]));
            const storage = h.client.storageInfo();
            const storageResult = storage.catch((error) => error);
            await drain();
            assert.deepEqual(log, ['storage', 'get:1']);
            firstGate.resolve();
            await first;
            await drain();
            assert.deepEqual(log, ['storage', 'get:1', 'close']);
            closeGate.resolve();
            await Promise.all([close, later]);
            assert.deepEqual(log, ['storage', 'get:1', 'close', 'get:2']);
            h.client.dispose(new Error('disposed storage'));
            assert.equal((await storageResult).message, 'disposed storage');
        } finally {
            firstGate.resolve();
            closeGate.resolve();
            storageGate.resolve({});
            h.dispose();
        }
    });

    test('postMessage failures reject every queued request without leaking listeners', async () => {
        const h = await harness({ stats: async () => ({}) });
        try {
            h.failRequests(new Error('post failed'));
            const results = await Promise.allSettled(Array.from({ length: 8 }, () => h.client.stats()));
            assert.ok(results.every((item) => item.status === 'rejected' && item.reason.message === 'post failed'));
            h.client.dispose();
            assert.equal(h.worker.listeners.size, 0);
        } finally {
            h.dispose();
        }
    });

    test('timeouts ignore late replies and disposal rejects remaining requests once', async () => {
        const never = deferred();
        const h = await harness({ stats: async () => never.promise }, { requestTimeoutMs: 5 });
        let settlements = 0;
        const activeTimers = new Set();
        const originalSetTimeout = globalThis.setTimeout;
        const originalClearTimeout = globalThis.clearTimeout;
        let createdTimers = 0;
        let clearedTimers = 0;
        globalThis.setTimeout = (handler, delay, ...args) => {
            let timer;
            timer = originalSetTimeout(() => {
                activeTimers.delete(timer);
                handler(...args);
            }, delay);
            createdTimers += 1;
            activeTimers.add(timer);
            return timer;
        };
        globalThis.clearTimeout = (timer) => {
            if (activeTimers.delete(timer)) clearedTimers += 1;
            originalClearTimeout(timer);
        };
        try {
            const request = h.client.stats().then(
                () => ++settlements,
                (error) => {
                    settlements += 1;
                    throw error;
                }
            );
            await assert.rejects(request, { name: 'WorkerRequestTimeoutError' });
            assert.equal(activeTimers.size, 0);
            const id = requests(h.toWorker)[0].id;
            h.injectResponse({ type: protocol.WORKER_PROTOCOL_RESPONSE, version: 1, id, ok: true, result: {} });
            await drain();
            assert.equal(settlements, 1);
            const pending = h.client.stats();
            const rejected = assert.rejects(pending, { message: 'disposed pending' });
            await drain();
            assert.equal(activeTimers.size, 1);
            h.client.dispose(new Error('disposed pending'));
            await rejected;
            assert.equal(h.worker.listeners.size, 0);
            assert.equal(activeTimers.size, 0);
            observations.timers = {
                created: createdTimers,
                clearedByDisposal: clearedTimers,
                remaining: activeTimers.size
            };
        } finally {
            never.resolve({});
            h.dispose();
            globalThis.setTimeout = originalSetTimeout;
            globalThis.clearTimeout = originalClearTimeout;
            for (const timer of activeTimers) originalClearTimeout(timer);
        }
    });

    test('fatal events before READY reject readiness and waiting requests', async () => {
        const h = await harness({}, { holdReady: true });
        let fatalities = 0;
        h.client.setFatalHandler(() => ++fatalities);
        try {
            const ready = assert.rejects(h.client.whenReady(), { message: 'worker died' });
            const pending = assert.rejects(h.client.stats(), { message: 'worker died' });
            h.worker.emit('error', null, { error: new Error('worker died') });
            await Promise.all([ready, pending]);
            h.worker.emit('error', null, { error: new Error('again') });
            assert.equal(fatalities, 1);
            assert.equal(h.worker.listeners.size, 0);
            assert.equal(h.toWorker.length, 0);
        } finally {
            h.dispose();
        }
    });

    test('a response transfer failure falls back once with intact bytes', async () => {
        const h = await harness(
            { exportSnapshot: async () => new Uint8Array([4, 5, 6]) },
            { failResponseTransfer: true }
        );
        try {
            assert.deepEqual(Array.from(await h.client.exportSnapshot()), [4, 5, 6]);
            assert.equal(h.failedTransferAttempts, 1);
            assert.equal(responses(h.toClient).filter((message) => message.id !== undefined).length, 1);
        } finally {
            h.dispose();
        }
    });

    test('legacy READY keeps single request and response envelopes compatible', async () => {
        const h = await harness({ stats: async () => ({ answer: 7 }) }, { legacyReady: true });
        try {
            assert.deepEqual(
                await Promise.all(Array.from({ length: 8 }, () => h.client.stats())),
                Array.from({ length: 8 }, () => ({ answer: 7 }))
            );
            assert.equal(h.toWorker.length, 8);
            assert.ok(h.toWorker.every(({ data }) => data.type === protocol.WORKER_PROTOCOL_REQUEST));
            assert.ok(
                h.toClient
                    .filter(({ data }) => data.id !== undefined)
                    .every(({ data }) => data.type === protocol.WORKER_PROTOCOL_RESPONSE)
            );
        } finally {
            h.dispose();
        }
    });

    test('unmatched and duplicate replies cannot settle another request', async () => {
        const gate = deferred();
        const h = await harness({ stats: async () => gate.promise });
        let settlements = 0;
        try {
            const first = h.client.stats().then((value) => {
                settlements += 1;
                return value;
            });
            const second = h.client.stats();
            const secondRejected = assert.rejects(second, { message: 'cancel second' });
            await drain();
            const id = requests(h.toWorker)[0].id;
            const reply = {
                type: protocol.WORKER_PROTOCOL_RESPONSE,
                version: 1,
                id,
                ok: true,
                result: { first: true }
            };
            h.injectResponse({ ...reply, id: 9999 });
            h.injectResponse(reply);
            h.injectResponse({ ...reply, result: { duplicate: true } });
            assert.deepEqual(await first, { first: true });
            assert.equal(settlements, 1);
            h.client.dispose(new Error('cancel second'));
            await secondRejected;
        } finally {
            gate.resolve({});
            h.dispose();
        }
    });

    test(
        'partial single-key requests transfer only the visible eight bytes',
        async () => {
            let receivedBackingBytes = 0;
            const h = await harness({
                get: async (_id, _store, key) => {
                    receivedBackingBytes += key.buffer.byteLength;
                    return key.slice();
                }
            });
            const caller = new Uint8Array(64 * 1024);
            const key = caller.subarray(200, 208);
            key.set([1, 2, 3, 4, 5, 6, 7, 8]);
            try {
                assert.deepEqual(Array.from(await h.client.get(1, 'kv', key)), [1, 2, 3, 4, 5, 6, 7, 8]);
                observations.partialKey = {
                    callerBackingBytes: caller.byteLength,
                    receivedBackingBytes,
                    transferredBytes: h.toWorker.reduce((total, item) => total + item.transferredBytes, 0)
                };
                assert.equal(receivedBackingBytes, 8, 'transport must not copy the caller backing allocation');
                assert.equal(
                    h.toWorker.reduce((total, item) => total + item.transferredBytes, 0),
                    8
                );
                assert.equal(caller.byteLength, 64 * 1024);
            } finally {
                h.dispose();
            }
        },
        'work'
    );

    test(
        'eight pending scalar requests use one request and one response message',
        async () => {
            let calls = 0;
            const h = await harness({ stats: async () => ({ sequence: ++calls }) });
            const originalClone = globalThis.structuredClone;
            let cloneCalls = 0;
            globalThis.structuredClone = (...args) => {
                cloneCalls += 1;
                return originalClone(...args);
            };
            try {
                const result = await Promise.all(Array.from({ length: 8 }, () => h.client.stats()));
                assert.deepEqual(
                    result.map((item) => item.sequence),
                    [1, 2, 3, 4, 5, 6, 7, 8]
                );
                const replyMessages = h.toClient.filter(({ data }) => data.type !== protocol.WORKER_PROTOCOL_READY);
                const wireCloneCalls = h.toWorker.length + replyMessages.length;
                observations.batch8 = {
                    requestMessages: h.toWorker.length,
                    responseMessages: replyMessages.length,
                    nativeStructuredClones: cloneCalls,
                    localSnapshotClones: cloneCalls - wireCloneCalls
                };
                assert.equal(h.toWorker.length, 1, 'eight pending commands should share one transport envelope');
                assert.equal(h.toWorker[0].data.type, REQUEST_BATCH);
                assert.equal(replyMessages.length, 1);
                assert.equal(replyMessages[0].data.type, RESPONSE_BATCH);
                const ids = responses(replyMessages).map((message) => message.id);
                assert.equal(new Set(ids).size, 8);
                assert.ok(cloneCalls <= 18, 'eight snapshots in each direction plus two message clones');
            } finally {
                globalThis.structuredClone = originalClone;
                h.dispose();
            }
        },
        'work'
    );

    test(
        'transaction scheduling registers only one cleanup reaction',
        async () => {
            let calls = 0;
            const h = await harness({
                get: async () => {
                    calls += 1;
                    return null;
                }
            });
            try {
                // Only the synchronous public message dispatch is observed. API
                // execution and response delivery happen in later microtasks.
                const before = __transportWorkCounter.thenRegistrations;
                h.scope.emit('message', {
                    type: protocol.WORKER_PROTOCOL_REQUEST,
                    version: 1,
                    id: 700,
                    command: 'get',
                    args: [1, 'kv', new Uint8Array([1])]
                });
                const registrations = __transportWorkCounter.thenRegistrations - before;
                await drain();
                assert.equal(calls, 1);
                observations.laneCleanupRegistrations = registrations;
                assert.equal(registrations, 2, 'one settlement reaction plus one combined cleanup');
            } finally {
                h.dispose();
            }
        },
        'work'
    );

    test(
        'eight sequential gets keep single envelopes and need no local snapshot clones',
        async () => {
            const h = await harness({ get: async (_id, _store, key) => key.slice() });
            const originalClone = globalThis.structuredClone;
            let cloneCalls = 0;
            globalThis.structuredClone = (...args) => {
                cloneCalls += 1;
                return originalClone(...args);
            };
            try {
                for (let index = 0; index < 8; index += 1) {
                    assert.deepEqual(Array.from(await h.client.get(1, 'kv', new Uint8Array([index]))), [index]);
                }
                const replyMessages = h.toClient.filter(({ data }) => data.type !== protocol.WORKER_PROTOCOL_READY);
                observations.sequential8 = {
                    requestMessages: h.toWorker.length,
                    responseMessages: replyMessages.length,
                    nativeStructuredClones: cloneCalls,
                    localSnapshotClones: cloneCalls - h.toWorker.length - replyMessages.length
                };
                assert.equal(h.toWorker.length, 8);
                assert.equal(replyMessages.length, 8);
                assert.ok(h.toWorker.every(({ data }) => data.type === protocol.WORKER_PROTOCOL_REQUEST));
                assert.equal(cloneCalls, 16, 'sequential calls should use only the existing message clones');
            } finally {
                globalThis.structuredClone = originalClone;
                h.dispose();
            }
        },
        'work'
    );

    test(
        'request envelopes never exceed 128 commands',
        async () => {
            const h = await harness({ stats: async () => ({}) });
            try {
                await Promise.all(Array.from({ length: 129 }, () => h.client.stats()));
                assert.equal(h.toWorker.length, 2);
                assert.deepEqual(
                    h.toWorker.map(({ data }) => data.requests?.length ?? 1),
                    [128, 1]
                );
            } finally {
                h.dispose();
            }
        },
        'work'
    );

    test(
        'batch byte budgets split large requests without changing execution order',
        async () => {
            const seen = [];
            const h = await harness({ put: async (_id, _store, key, value) => seen.push([key[0], value.length]) });
            try {
                await Promise.all([
                    h.client.put(1, 'kv', new Uint8Array([1]), new Uint8Array(700 * 1024)),
                    h.client.put(1, 'kv', new Uint8Array([2]), new Uint8Array(700 * 1024)),
                    h.client.put(1, 'kv', new Uint8Array([3]), new Uint8Array(4))
                ]);
                assert.deepEqual(seen, [
                    [1, 700 * 1024],
                    [2, 700 * 1024],
                    [3, 4]
                ]);
                assert.equal(h.toWorker.length, 2, 'one MiB budget should split the two 700 KiB values');
                assert.ok(h.toWorker.every((item) => item.transferredBytes <= 1024 * 1024));
            } finally {
                h.dispose();
            }
        },
        'work'
    );

    for (const [name, response] of [
        [
            'corrupt packed response',
            {
                ok: true,
                result: { __moyodbPacked: 'moyodb:packed-optional-values:v1', bytes: new Uint8Array([0, 0, 0, 0, 1]) }
            }
        ],
        ['malformed error response', { ok: false, error: null }]
    ]) {
        test(
            `${name} rejects its request instead of throwing and leaving it pending`,
            async () => {
                const gate = deferred();
                const h = await harness({ getMany: async () => gate.promise });
                let outcome = { status: 'pending' };
                try {
                    void h.client.getMany(1, 'kv', []).then(
                        (value) => (outcome = { status: 'fulfilled', value }),
                        (error) => (outcome = { status: 'rejected', error })
                    );
                    await drain();
                    const id = requests(h.toWorker)[0].id;
                    h.injectResponse({ type: protocol.WORKER_PROTOCOL_RESPONSE, version: 1, id, ...response });
                    await drain();
                    assert.equal(outcome.status, 'rejected', 'malformed wire payload must settle the pending request');
                    assert.equal(outcome.error.name, 'WorkerProtocolError');
                    assert.equal(h.worker.errors.length, 0, 'message handler must not throw into the host');
                } finally {
                    gate.resolve([]);
                    h.dispose();
                }
            },
            'regressions'
        );
    }

    return {
        async runTests(mode = 'all') {
            let passed = 0;
            const failures = [];
            for (const { name, run, kind } of tests) {
                if (mode !== 'all' && mode !== kind) continue;
                try {
                    await run();
                    passed += 1;
                } catch (error) {
                    failures.push({ name, kind, error: error.stack ?? String(error) });
                }
            }
            return { passed, failed: failures.length, observations, failures };
        }
    };
}
