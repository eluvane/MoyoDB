import {
    compoundKey,
    compoundKeyRange,
    deleteDB,
    indexKey,
    jsonDecode,
    jsonEncode,
    openDB,
    prefixRange,
    prefixSuccessor,
    splitCompoundKey,
    unsafeDebugCrashWorker,
    utf8Decode,
    utf8Encode,
    type DB
} from './index';

declare global {
    interface Window {
        moyodb: {
            openDB: typeof openDB;
            deleteDB: typeof deleteDB;
            utf8Encode: typeof utf8Encode;
            utf8Decode: typeof utf8Decode;
            jsonEncode: typeof jsonEncode;
            jsonDecode: typeof jsonDecode;
            indexKey: typeof indexKey;
            compoundKey: typeof compoundKey;
            splitCompoundKey: typeof splitCompoundKey;
            prefixRange: typeof prefixRange;
            prefixSuccessor: typeof prefixSuccessor;
            compoundKeyRange: typeof compoundKeyRange;
            unsafeDebugCrashWorker: typeof unsafeDebugCrashWorker;
        };
    }
}

window.moyodb = {
    openDB,
    deleteDB,
    utf8Encode,
    utf8Decode,
    jsonEncode,
    jsonDecode,
    indexKey,
    compoundKey,
    splitCompoundKey,
    prefixRange,
    prefixSuccessor,
    compoundKeyRange,
    unsafeDebugCrashWorker
};

const DEMO_DB = 'moyodb-demo';
const BULK_DEMO_DB = `${DEMO_DB}-bulk`;
const IDB_DEMO_DB = `${DEMO_DB}-idb`;
const OPFS_DEMO_ROOTS = ['stackdb', '__moyodb_bench_raw__', '__moyodb_bench_support__'];
const STORE = 'kv';
let lastBenchmarkJson: unknown = null;

const environment = document.getElementById('environment') as HTMLPreElement;
const output = document.getElementById('output') as HTMLPreElement;
const exportButton = document.getElementById('export-json') as HTMLButtonElement;

void renderEnvironment();

bind('open-db', async () => {
    const db = await ensureDemoDb();
    try {
        const stats = await db.stats();
        print({ opened: DEMO_DB, stats });
    } finally {
        await db.close();
    }
});

bind('put-get', async () => {
    const db = await ensureDemoDb();
    try {
        await db.put(STORE, utf8Encode('hello'), utf8Encode('moyodb'));
        const value = await db.get(STORE, utf8Encode('hello'));
        print({ key: 'hello', value: value ? utf8Decode(value) : null });
    } finally {
        await db.close();
    }
});

bind('range-scan', async () => {
    const db = await ensureDemoDb();
    try {
        const tx = await db.begin('readwrite');
        try {
            const entries: Array<[Uint8Array, Uint8Array]> = [];
            for (let i = 0; i < 10; i += 1) {
                entries.push([utf8Encode(`scan:${i.toString().padStart(2, '0')}`), utf8Encode(`value:${i}`)]);
            }
            await tx.putMany(STORE, entries);
            await tx.commit();
        } catch (error) {
            await tx.rollback().catch(() => undefined);
            throw error;
        }
        const rows = await db.scan(STORE, {
            gte: utf8Encode('scan:03'),
            lte: utf8Encode('scan:07')
        });
        print(rows.map((row) => ({ key: utf8Decode(row.key), value: utf8Decode(row.value) })));
    } finally {
        await db.close();
    }
});

bind('bulk-insert', async () => {
    await deleteMoyoDbIfExists(BULK_DEMO_DB);
    let db: DB | null = null;
    try {
        db = await ensureDemoDb(BULK_DEMO_DB);
        const openedDb = db;
        const result = await time('moyodb_bulk_insert_10k', async () => {
            await bulkInsertMoyo(openedDb, 10_000, 1000, 128);
        });
        lastBenchmarkJson = {
            generatedAt: new Date().toISOString(),
            browser: await browserInfo(),
            results: [result],
            notes: ['Minimal demo benchmark; use packages/sdk/bench for reproducible benchmark reports.']
        };
        exportButton.disabled = false;
        print(lastBenchmarkJson);
    } finally {
        await db?.close().catch(() => undefined);
        await deleteMoyoDbIfExists(BULK_DEMO_DB);
    }
});

bind('bulk-insert-idb', async () => {
    await deleteIndexedDb(IDB_DEMO_DB);
    try {
        const result = await time('indexeddb_bulk_insert_10k', async () => {
            await bulkInsertIndexedDb(IDB_DEMO_DB, 10_000, 1000, 128);
        });
        lastBenchmarkJson = {
            generatedAt: new Date().toISOString(),
            browser: await browserInfo(),
            results: [result],
            notes: ['Minimal IndexedDB demo benchmark; use packages/sdk/bench for reproducible benchmark reports.']
        };
        exportButton.disabled = false;
        print(lastBenchmarkJson);
    } finally {
        await deleteIndexedDb(IDB_DEMO_DB);
    }
});

bind('compare-idb', async () => {
    await deleteMoyoDbIfExists(BULK_DEMO_DB);
    let moyo: DB | null = null;
    try {
        moyo = await ensureDemoDb(BULK_DEMO_DB);
        const openedMoyo = moyo;
        await deleteIndexedDb(IDB_DEMO_DB);
        const results = [];
        results.push(
            await time('moyodb_bulk_insert_10k', async () => {
                await bulkInsertMoyo(openedMoyo, 10_000, 1000, 128);
            })
        );
        results.push(
            await time('indexeddb_bulk_insert_10k', async () => {
                await bulkInsertIndexedDb(IDB_DEMO_DB, 10_000, 1000, 128);
            })
        );
        lastBenchmarkJson = {
            generatedAt: new Date().toISOString(),
            browser: await browserInfo(),
            results,
            notes: [
                'Minimal demo comparison only.',
                'Use npm run bench:browser for raw samples, percentiles, warmups, and comparable transaction boundaries.'
            ]
        };
        exportButton.disabled = false;
        print(lastBenchmarkJson);
    } finally {
        await moyo?.close().catch(() => undefined);
        await deleteMoyoDbIfExists(BULK_DEMO_DB);
        await deleteIndexedDb(IDB_DEMO_DB);
    }
});

bind('clear-storage', async () => {
    const before = await storageEstimate();
    await deleteMoyoDbIfExists(DEMO_DB);
    await deleteMoyoDbIfExists(BULK_DEMO_DB);
    await deleteIndexedDb(IDB_DEMO_DB);
    const opfs = [];
    for (const name of OPFS_DEMO_ROOTS) {
        opfs.push(await removeOpfsRoot(name));
    }
    const after = await storageEstimate();
    lastBenchmarkJson = null;
    exportButton.disabled = true;
    print({ cleared: true, before, after, opfs });
});

exportButton.addEventListener('click', () => {
    if (!lastBenchmarkJson) return;
    downloadText('moyodb-demo-benchmark.json', JSON.stringify(lastBenchmarkJson, null, 2), 'application/json');
});

async function ensureDemoDb(name = DEMO_DB): Promise<DB> {
    const db = await openDB(name, { requestPersistence: false });
    const stores = await db.listStores();
    if (!stores.includes(STORE)) {
        await db.createStore(STORE);
    }
    return db;
}

async function bulkInsertMoyo(db: DB, count: number, batchSize: number, valueSize: number): Promise<void> {
    for (let start = 0; start < count; start += batchSize) {
        const tx = await db.begin('readwrite');
        try {
            const entries: Array<[Uint8Array, Uint8Array]> = [];
            for (let i = start; i < Math.min(start + batchSize, count); i += 1) {
                entries.push([utf8Encode(fixedKey(i)), fixedValue(i, valueSize)]);
            }
            await tx.putMany(STORE, entries);
            await tx.commit();
        } catch (error) {
            await tx.rollback().catch(() => undefined);
            throw error;
        }
    }
}

async function bulkInsertIndexedDb(name: string, count: number, batchSize: number, valueSize: number): Promise<void> {
    const db = await openIndexedDb(name);
    try {
        for (let start = 0; start < count; start += batchSize) {
            await new Promise<void>((resolve, reject) => {
                const tx = db.transaction(STORE, 'readwrite');
                const store = tx.objectStore(STORE);
                tx.oncomplete = () => resolve();
                tx.onerror = () => reject(tx.error ?? new Error('IndexedDB transaction failed'));
                tx.onabort = () => reject(tx.error ?? new Error('IndexedDB transaction aborted'));
                for (let i = start; i < Math.min(start + batchSize, count); i += 1) {
                    store.put(fixedValue(i, valueSize), fixedKey(i));
                }
            });
        }
    } finally {
        db.close();
    }
}

function openIndexedDb(name: string): Promise<IDBDatabase> {
    return new Promise((resolve, reject) => {
        const request = indexedDB.open(name, 1);
        request.onupgradeneeded = () => {
            const db = request.result;
            if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE);
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error ?? new Error(`open IndexedDB ${name} failed`));
    });
}

function deleteIndexedDb(name: string): Promise<void> {
    return new Promise((resolve) => {
        const request = indexedDB.deleteDatabase(name);
        request.onsuccess = () => resolve();
        request.onerror = () => resolve();
        request.onblocked = () => resolve();
    });
}

async function deleteMoyoDbIfExists(name: string): Promise<void> {
    await deleteDB(name).catch(() => undefined);
}

async function removeOpfsRoot(name: string): Promise<Record<string, unknown>> {
    const storage = navigator.storage as StorageManager & {
        getDirectory?: () => Promise<FileSystemDirectoryHandle>;
    };
    if (typeof storage.getDirectory !== 'function') {
        return { name, removed: false, error: 'navigator.storage.getDirectory is unavailable' };
    }
    try {
        const root = await storage.getDirectory();
        await root.removeEntry(name, { recursive: true });
        return { name, removed: true };
    } catch (error) {
        const reason = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
        if (error instanceof DOMException && error.name === 'NotFoundError') {
            return { name, removed: false, reason };
        }
        return { name, removed: false, error: reason };
    }
}

async function time(name: string, fn: () => Promise<void>): Promise<Record<string, unknown>> {
    const started = performance.now();
    await fn();
    const durationMs = performance.now() - started;
    return { name, durationMs, recordCount: 10_000, valueSize: 128, batchSize: 1000 };
}

function fixedKey(index: number): string {
    return `k${index.toString().padStart(12, '0')}`;
}

function fixedValue(index: number, size: number): Uint8Array {
    const bytes = new Uint8Array(size);
    for (let i = 0; i < bytes.length; i += 1) bytes[i] = (index + i) & 0xff;
    return bytes;
}

async function renderEnvironment(): Promise<void> {
    environment.textContent = JSON.stringify(await browserInfo(), null, 2);
}

async function browserInfo(): Promise<Record<string, unknown>> {
    const storage = navigator.storage as StorageManager & {
        getDirectory?: () => Promise<unknown>;
        estimate?: () => Promise<StorageEstimate>;
        persisted?: () => Promise<boolean>;
    };
    return {
        userAgent: navigator.userAgent,
        platform: navigator.platform,
        secureContext: window.isSecureContext,
        opfsSupported: typeof storage.getDirectory === 'function',
        storageEstimate: typeof storage.estimate === 'function' ? await storage.estimate().catch(() => null) : null,
        persisted: typeof storage.persisted === 'function' ? await storage.persisted().catch(() => false) : false
    };
}

async function storageEstimate(): Promise<StorageEstimate | null> {
    const storage = navigator.storage as StorageManager & {
        estimate?: () => Promise<StorageEstimate>;
    };
    return typeof storage.estimate === 'function' ? await storage.estimate().catch(() => null) : null;
}

function bind(id: string, fn: () => Promise<void>): void {
    document.getElementById(id)?.addEventListener('click', () => {
        output.textContent = 'running...';
        fn().catch((error) => {
            output.textContent =
                error instanceof Error ? `${error.name}: ${error.message}\n${error.stack ?? ''}` : String(error);
        });
    });
}

function print(value: unknown): void {
    output.textContent = JSON.stringify(value, null, 2);
}

function downloadText(filename: string, text: string, type: string): void {
    const blob = new Blob([text], { type });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = filename;
    document.body.append(anchor);
    anchor.click();
    anchor.remove();
    URL.revokeObjectURL(url);
}
