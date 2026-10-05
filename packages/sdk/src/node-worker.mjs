import { readFile } from 'node:fs/promises';
import { BroadcastChannel, parentPort, workerData } from 'node:worker_threads';
import { installNodeStorage } from './node-storage.mjs';

if (!parentPort) {
    throw new Error('Node database runtime must run in a worker thread');
}

const listeners = new Map();
const scope = {
    isSecureContext: true,
    location: { origin: 'moyodb-node' },
    addEventListener(type, listener) {
        let entries = listeners.get(type);
        if (!entries) {
            entries = new Set();
            listeners.set(type, entries);
        }
        entries.add(listener);
    },
    removeEventListener(type, listener) {
        listeners.get(type)?.delete(listener);
    },
    postMessage(data, transfer = []) {
        parentPort.postMessage(data, transfer);
    }
};

parentPort.on('message', (data) => {
    const event = { data, origin: '', ports: [] };
    for (const listener of Array.from(listeners.get('message') ?? [])) {
        if (typeof listener === 'function') listener(event);
        else listener.handleEvent(event);
    }
});

Object.defineProperty(globalThis, 'self', { configurable: true, value: scope });

class NodeBroadcastChannel extends BroadcastChannel {
    constructor(name) {
        const channelName = name === `db:${workerData.dbName}:events` ? `db:${workerData.channelName}:events` : name;
        super(channelName);
    }
}

Object.defineProperty(globalThis, 'BroadcastChannel', { configurable: true, value: NodeBroadcastChannel });

let storage;
let server;

async function acquireStorage() {
    const deadline = performance.now() + workerData.ownerWaitMs;
    for (;;) {
        try {
            return await installNodeStorage(workerData.directory, {
                lockToken: workerData.lockToken,
                encodedDbName: workerData.encodedDbName
            });
        } catch (error) {
            const remaining = deadline - performance.now();
            if (error.name !== 'DatabaseBusyError' || remaining <= 0) throw error;
            await new Promise((resolve) => setTimeout(resolve, Math.min(remaining, 25)));
        }
    }
}

try {
    storage = await acquireStorage();
    // The file lease excludes other owners before this lock is granted.
    Object.defineProperty(navigator, 'locks', {
        configurable: true,
        value: {
            async request(name, options, callback) {
                options.signal?.throwIfAborted();
                return callback({ name, mode: options.mode ?? 'exclusive' });
            }
        }
    });
    // The filesystem adapter supplies the actual handles.
    class NodeFileSystemFileHandle {
        createSyncAccessHandle() {}
    }
    Object.defineProperty(globalThis, 'FileSystemFileHandle', {
        configurable: true,
        value: NodeFileSystemFileHandle
    });
    const { DbWorker, exposeWorkerApi } = await import(new URL('./node-engine.js', import.meta.url).href);
    const engine = new DbWorker({
        async loadWasm() {
            const module = await import(new URL('./engine/moyodb_engine.js', import.meta.url).href);
            const bytes = await readFile(new URL('./engine/moyodb_engine_bg.wasm', import.meta.url));
            await module.default({ module_or_path: bytes });
            return module;
        },
        persistence: {
            async persisted() {
                return true;
            },
            async persist() {
                return true;
            },
            close() {}
        }
    });
    server = exposeWorkerApi(engine, scope);
    process.once('exit', () => {
        server.close();
        storage.close();
    });
} catch (error) {
    server?.close();
    try {
        storage?.close();
    } catch {
        // Preserve the runtime failure.
    }
    throw error;
}
