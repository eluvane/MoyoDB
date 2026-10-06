import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { MessageChannel } from 'node:worker_threads';
import { test } from 'node:test';
import ts from 'typescript';

const modules = new Map();
async function moduleUrl(name) {
    if (modules.has(name)) return modules.get(name);
    const source = await readFile(new URL(`../src/${name}.ts`, import.meta.url), 'utf8');
    let code = ts.transpileModule(source, {
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
    }).outputText;
    for (const dependency of new Set(Array.from(code.matchAll(/from '\.\/([^']+)'/g), (match) => match[1]))) {
        code = code.replaceAll(`from './${dependency}'`, `from '${await moduleUrl(dependency)}'`);
    }
    const url = `data:text/javascript;base64,${Buffer.from(`${code}\n//# sourceURL=moyodb-test://${name}.ts`).toString('base64')}`;
    modules.set(name, url);
    return url;
}
const { SharedWorkerCoordinator } = await import(await moduleUrl('shared-worker'));
const { WorkerProtocolClient } = await import(await moduleUrl('worker-client'));
const { SharedWorkerProtocolClient } = await import(await moduleUrl('shared-worker-client'));
const { exposeWorkerApi } = await import(await moduleUrl('worker-server'));
const control = await import(await moduleUrl('shared-worker-protocol'));
const deferred = () => {
    let complete;
    const promise = new Promise((resolve) => {
        complete = resolve;
    });
    return { promise, resolve: complete };
};
const scope = (port) => ({
    location: { origin: 'https://cursor.test' },
    postMessage: (data, transfer = []) => port.postMessage(data, transfer),
    addEventListener: (type, listener) => port.addEventListener(type, listener),
    removeEventListener: (type, listener) => port.removeEventListener(type, listener)
});
const request = (options = {}) => ({ store: 'kv', range: {}, maxRows: 1, maxBytes: 1024, ...options });
const openRequest = {
    dbName: 'cursor-test',
    options: {
        createIfMissing: true,
        ownerWaitMs: 0,
        requestPersistence: false,
        cachePages: 256,
        debugFailpoint: null,
        changeFeed: null
    }
};

function ownerApi() {
    const transactions = new Map();
    const cursors = new Map();
    const scans = [];
    const indexScans = [];
    let nextTx = 100;
    let nextCursor = 1000;
    let gate;
    let closeGate;
    const api = {
        open: async () => {},
        close: async () => {
            transactions.clear();
            cursors.clear();
        },
        begin: async (mode) => {
            const id = nextTx++;
            transactions.set(id, mode);
            return id;
        },
        commit: async (id) => {
            assert.ok(transactions.delete(id));
            for (const [cursor, state] of cursors) if (state.txId === id) cursors.delete(cursor);
            return id;
        },
        rollback: async (id) => {
            await api.commit(id);
        },
        async scanPage(input) {
            scans.push({ ...input });
            if (gate) {
                const current = gate;
                gate = undefined;
                current.entered.resolve();
                await current.release.promise;
            }
            let cursorId = input.cursorId;
            let state = cursors.get(cursorId);
            if (cursorId !== undefined && !state) throw Object.assign(new Error(), { name: 'TransactionClosedError' });
            if (!state) {
                const txId = input.txId ?? (await api.begin('readonly'));
                assert.ok(transactions.has(txId), 'explicit transaction ID must reach its owner');
                cursorId = nextCursor++;
                state = { txId, callerTxId: input.txId, ownsTx: input.txId === undefined, offset: 1 };
                cursors.set(cursorId, state);
            }
            assert.equal(state.callerTxId, input.txId);
            assert.ok(transactions.has(state.txId), 'page must finish before transaction close');
            const rows = [{ key: Uint8Array.of(state.offset++), value: Uint8Array.of(state.txId) }];
            return { rows, cursorId, done: false, bytes: 14 };
        },
        async closeCursor(id) {
            if (closeGate) {
                const current = closeGate;
                closeGate = undefined;
                current.entered.resolve();
                await current.release.promise;
            }
            const state = cursors.get(id);
            cursors.delete(id);
            if (state?.ownsTx) transactions.delete(state.txId);
        },
        async scanByIndexPage(...args) {
            assert.ok(transactions.has(args[0]));
            indexScans.push(args);
            return { rows: [{ key: Uint8Array.of(3), value: Uint8Array.of(4, 5) }], cursor: Uint8Array.of(6) };
        }
    };
    return {
        api,
        transactions,
        cursors,
        scans,
        indexScans,
        blockPage() {
            gate = { entered: deferred(), release: deferred() };
            return { entered: gate.entered.promise, release: gate.release.resolve };
        },
        blockCursorClose() {
            closeGate = { entered: deferred(), release: deferred() };
            return { entered: closeGate.entered.promise, release: closeGate.release.resolve };
        }
    };
}

async function fixture(shared = true) {
    const owner = ownerApi();
    const ports = new Set();
    const clients = [];
    const portCloses = new Map();
    const connections = new Map();
    const channels = () => {
        const channel = new MessageChannel();
        ports.add(channel.port1);
        ports.add(channel.port2);
        return channel;
    };
    let server;
    const coordinator = shared ? new SharedWorkerCoordinator('https://cursor.test') : null;
    const connect = async (sharedClient = false) => {
        const channel = channels();
        if (coordinator) {
            channel.port1.addEventListener('message', (event) => {
                if (event.data.type !== control.SHARED_WORKER_OWNER_REQUEST) return;
                const storage = channels();
                const host = channels();
                server = exposeWorkerApi(owner.api, scope(storage.port1));
                storage.port1.start();
                channel.port1.postMessage({ type: control.SHARED_WORKER_OWNER_RESPONSE, id: event.data.id, ok: true }, [
                    storage.port2,
                    host.port2
                ]);
            });
            coordinator.connect(channel.port2);
        } else {
            server = exposeWorkerApi(owner.api, scope(channel.port2));
            channel.port2.start();
        }
        let client;
        if (sharedClient) {
            const events = new EventTarget();
            client = new SharedWorkerProtocolClient({
                port: {
                    postMessage: (data, transfer = []) => channel.port1.postMessage(data, transfer),
                    addEventListener: (type, listener) => channel.port1.addEventListener(type, listener),
                    removeEventListener: (type, listener) => channel.port1.removeEventListener(type, listener),
                    start: () => channel.port1.start(),
                    close: () => {
                        portCloses.set(client, (portCloses.get(client) ?? 0) + 1);
                        channel.port1.close();
                    }
                },
                addEventListener: (type, listener) => events.addEventListener(type, listener),
                removeEventListener: (type, listener) => events.removeEventListener(type, listener)
            });
        } else {
            client = new WorkerProtocolClient(channel.port1, { requestTimeoutMs: 2000, readyTimeoutMs: 2000 });
        }
        channel.port1.start();
        clients.push(client);
        connections.set(client, {
            port: channel.port1,
            session: coordinator && Array.from(coordinator.sessions).find((session) => session.port === channel.port2)
        });
        await client.open(openRequest);
        return client;
    };
    return {
        ...owner,
        connect,
        portCloses,
        abandon(client) {
            const connection = connections.get(client);
            assert.ok(coordinator && connection.session);
            client.dispose();
            connection.port.close();
            clients.splice(clients.indexOf(client), 1);
            return async () => {
                const now = Date.now();
                coordinator.owner.lastSeen = now;
                for (const session of coordinator.sessions) {
                    session.lastSeen = session === connection.session ? now - control.SHARED_WORKER_LEASE_MS - 1 : now;
                }
                coordinator.expireSessions();
                await connection.session.closingPromise;
            };
        },
        forget(client) {
            const index = clients.indexOf(client);
            assert.ok(index >= 0);
            clients.splice(index, 1);
        },
        async disconnect(client) {
            await client.close();
            client.dispose();
            clients.splice(clients.indexOf(client), 1);
        },
        async close() {
            const results = await Promise.allSettled(
                clients.map(async (client) => {
                    try {
                        await client.close();
                    } finally {
                        client.dispose();
                    }
                })
            );
            coordinator?.failAll(new Error('fixture finished'));
            server?.close();
            for (const port of ports) port.close();
            const failure = results.find((result) => result.status === 'rejected');
            if (failure) throw failure.reason;
        }
    };
}

test('shared primary cursors translate nested transaction IDs and reject foreign transport calls', async () => {
    const env = await fixture();
    try {
        const first = await env.connect();
        const second = await env.connect();
        const tx = await first.begin('readonly');
        assert.equal(tx, 1);
        const page = await first.scanPage(request({ txId: tx }));
        assert.equal(env.scans[0].txId, 100);
        assert.equal(page.cursorId, 1);
        await assert.rejects(second.scanPage(request({ cursorId: page.cursorId })), { name: 'TransactionClosedError' });
        await assert.rejects(second.closeCursor(page.cursorId), { name: 'TransactionClosedError' });
        const next = await first.scanPage(request({ txId: tx, cursorId: page.cursorId }));
        assert.equal(next.rows[0].key[0], 2);
        assert.equal(env.scans.at(-1).cursorId, 1000);
        await first.closeCursor(page.cursorId);
        assert.equal(env.transactions.size, 1);
        await first.rollback(tx);
        assert.equal(env.transactions.size, 0);
    } finally {
        await env.close();
    }
});

test('shared client close releases only its autonomous cursor snapshots', async () => {
    const env = await fixture();
    try {
        const first = await env.connect();
        const second = await env.connect();
        await first.scanPage(request());
        const peerPage = await second.scanPage(request());
        assert.equal(env.transactions.size, 2);
        await env.disconnect(first);
        assert.equal(env.transactions.size, 1);
        const next = await second.scanPage(request({ cursorId: peerPage.cursorId }));
        assert.equal(next.rows[0].key[0], 2);
        await second.closeCursor(next.cursorId);
        assert.equal(env.transactions.size, 0);
        assert.equal(env.cursors.size, 0);
    } finally {
        await env.close();
    }
});

test('shared secondary pages translate transaction IDs and preserve the byte budget', async () => {
    const env = await fixture();
    try {
        const client = await env.connect();
        const txId = await client.begin('readonly');
        const page = await client.scanByIndexPage(txId, 'kv', 'byValue', {}, null, 2, 64);
        assert.deepEqual(env.indexScans, [[100, 'kv', 'byValue', {}, null, 2, 64]]);
        assert.deepEqual(page, {
            rows: [{ key: Uint8Array.of(3), value: Uint8Array.of(4, 5) }],
            cursor: Uint8Array.of(6)
        });
        await client.rollback(txId);
        assert.equal(env.transactions.size, 0);
    } finally {
        await env.close();
    }
});

test('page unload keeps the shared port open until autonomous cursor cleanup is acknowledged', async () => {
    const env = await fixture();
    let gate;
    try {
        await env.connect();
        const client = await env.connect(true);
        await client.scanPage(request());
        gate = env.blockCursorClose();
        let disconnect;
        client.setFatalHandler(() => {
            disconnect = client.disconnect();
        });
        env.forget(client);
        client.handlePageHide();
        await gate.entered;
        assert.equal(env.transactions.size, 1);
        assert.equal(env.portCloses.get(client) ?? 0, 0);
        gate.release();
        await disconnect;
        assert.equal(env.transactions.size, 0);
        assert.equal(env.cursors.size, 0);
        assert.equal(env.portCloses.get(client), 1);
    } finally {
        gate?.release();
        await env.close();
    }
});

test('a lost shared port releases only its cursor snapshot when the existing lease expires', async () => {
    const env = await fixture();
    try {
        const first = await env.connect();
        const second = await env.connect();
        const peer = await first.scanPage(request());
        await second.scanPage(request());
        const expire = env.abandon(second);
        assert.equal(env.transactions.size, 2);
        assert.equal(env.cursors.size, 2);
        await expire();
        assert.equal(env.transactions.size, 1);
        assert.equal(env.cursors.size, 1);
        const next = await first.scanPage(request({ cursorId: peer.cursorId }));
        assert.equal(next.rows[0].key[0], 2);
        assert.equal(next.rows[0].value[0], peer.rows[0].value[0]);
        await first.closeCursor(next.cursorId);
        assert.equal(env.transactions.size, 0);
    } finally {
        await env.close();
    }
});

test('explicit page decode stays in the transaction lane before commit', async () => {
    const env = await fixture(false);
    try {
        const client = await env.connect();
        const txId = await client.begin('readonly');
        const gate = env.blockPage();
        const page = client.scanPage(request({ txId }));
        await gate.entered;
        const commit = client.commit(txId);
        await new Promise((resolve) => setImmediate(resolve));
        assert.equal(env.transactions.size, 1);
        gate.release();
        await Promise.all([page, commit]);
        assert.equal(env.transactions.size, 0);
    } finally {
        await env.close();
    }
});

test('implicit continuation and close use the same cursor lane', async () => {
    const env = await fixture(false);
    try {
        const client = await env.connect();
        const first = await client.scanPage(request());
        const gate = env.blockPage();
        const page = client.scanPage(request({ cursorId: first.cursorId }));
        await gate.entered;
        const close = client.closeCursor(first.cursorId);
        await new Promise((resolve) => setImmediate(resolve));
        assert.equal(env.transactions.size, 1);
        gate.release();
        await Promise.all([page, close]);
        assert.equal(env.transactions.size, 0);
    } finally {
        await env.close();
    }
});
