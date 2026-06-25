import type { SampleContext, WorkloadRunner, WorkloadSpec } from './types';
import { withTimeout } from '../src/internal';
import {
    DeterministicRng,
    isBulkInsertWorkload,
    isBulkRandomGetWorkload,
    isRandomGetWorkload,
    isRangeScanWorkload,
    isSingleTransactionInsertWorkload,
    keyString,
    randomReadCount,
    scanWindow,
    STORE_NAME,
    valueBytes
} from './workloads';

type IdbEntry = [string, Uint8Array];

interface IndexedDbHandle {
    db: IDBDatabase;
    name: string;
}

type PreparedIndexedDbSample = {
    db?: IDBDatabase;
    entries?: IdbEntry[][];
    readKeys?: string[];
    cleanupNames?: string[];
};

export const indexedDbBaseline: WorkloadRunner = {
    engine: 'indexeddb',
    async prepare(ctx: SampleContext): Promise<(() => Promise<void>) | void> {
        if (!ctx.workload.supports.includes('indexeddb')) {
            return undefined;
        }
        await requireIndexedDbCapabilities();
        const prepared: PreparedIndexedDbSample = { cleanupNames: [ctx.dbName] };

        if (ctx.workload.name === 'open_empty_db') {
            await deleteIndexedDb(ctx.dbName);
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (
            ctx.workload.name === 'indexeddb_bulk_put_10k' ||
            isBulkInsertWorkload(ctx.workload.name) ||
            ctx.workload.name === 'small_tx_1000_commits'
        ) {
            await deleteIndexedDb(ctx.dbName);
            prepared.db = await openDb(ctx.dbName, true);
            prepared.entries = buildEntryBatches(
                ctx.workload,
                ctx.workload.recordCount,
                effectiveBatchSize(ctx.workload)
            );
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (isRandomGetWorkload(ctx.workload.name) || isRangeScanWorkload(ctx.workload.name)) {
            await deleteIndexedDb(ctx.dbName);
            prepared.db = await openDb(ctx.dbName, true);
            const entries = buildEntryBatches(ctx.workload, ctx.workload.recordCount, effectiveBatchSize(ctx.workload));
            await bulkInsertPrepared(prepared.db, entries, shouldUseSingleTransactionPreload(ctx.workload));
            if (isRandomGetWorkload(ctx.workload.name)) {
                prepared.readKeys = buildRandomReadKeys(ctx.workload, ctx.sampleIndex);
            }
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (ctx.workload.name === 'cold_open_after_100k') {
            await deleteIndexedDb(ctx.dbName);
            const db = await openDb(ctx.dbName, true);
            try {
                const entries = buildEntryBatches(
                    ctx.workload,
                    ctx.workload.recordCount,
                    effectiveBatchSize(ctx.workload)
                );
                await bulkInsertPrepared(db, entries, false);
            } finally {
                db.close();
            }
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        return undefined;
    },
    async run(ctx: SampleContext): Promise<void> {
        if (!ctx.workload.supports.includes('indexeddb')) {
            throw new NotApplicableError(`IndexedDB baseline is not applicable to ${ctx.workload.name}`);
        }
        await requireIndexedDbCapabilities();
        await runIndexedDbWorkload(ctx.dbName, ctx.workload);
    },
    async cleanup(ctx: SampleContext): Promise<void> {
        await cleanupPrepared(ctx.dbName);
    }
};

export class NotApplicableError extends Error {
    constructor(message: string) {
        super(message);
        this.name = 'NotApplicableError';
    }
}

const preparedSamples = new Map<string, PreparedIndexedDbSample>();
let capabilityProbe: Promise<void> | null = null;

async function requireIndexedDbCapabilities(): Promise<void> {
    capabilityProbe ??= probeIndexedDbCapabilities();
    return capabilityProbe;
}

async function probeIndexedDbCapabilities(): Promise<void> {
    if (typeof indexedDB === 'undefined') {
        throw new NotApplicableError('IndexedDB is unavailable.');
    }
    const ok = await withTimeout(probeIndexedDbRoundtrip(`__moyodb_idb_probe__-${Date.now()}`), 3000, false);
    if (!ok) {
        throw new NotApplicableError('IndexedDB roundtrip probe did not complete.');
    }
}

function probeIndexedDbRoundtrip(name: string): Promise<boolean> {
    return new Promise((resolve) => {
        const request = indexedDB.open(name, 1);
        request.onupgradeneeded = () => {
            const db = request.result;
            if (!db.objectStoreNames.contains(STORE_NAME)) {
                db.createObjectStore(STORE_NAME);
            }
        };
        request.onerror = () => resolve(false);
        request.onblocked = () => resolve(false);
        request.onsuccess = () => {
            const db = request.result;
            const tx = db.transaction(STORE_NAME, 'readwrite');
            tx.oncomplete = () => {
                db.close();
                const deleteRequest = indexedDB.deleteDatabase(name);
                deleteRequest.onsuccess = () => resolve(true);
                deleteRequest.onerror = () => resolve(true);
                deleteRequest.onblocked = () => resolve(true);
            };
            tx.onerror = () => {
                db.close();
                resolve(false);
            };
            tx.onabort = () => {
                db.close();
                resolve(false);
            };
            tx.objectStore(STORE_NAME).put(new Uint8Array([1]), 'probe');
        };
    });
}

async function runIndexedDbWorkload(dbName: string, workload: WorkloadSpec): Promise<void> {
    if (workload.name === 'open_empty_db') {
        await openEmptyDbMeasured(dbName);
        return;
    }

    if (
        workload.name === 'indexeddb_bulk_put_10k' ||
        isBulkInsertWorkload(workload.name) ||
        workload.name === 'small_tx_1000_commits'
    ) {
        const prepared = requirePrepared(dbName);
        const db = requirePreparedDb(prepared, dbName);
        const entries = requirePreparedEntries(prepared, dbName);
        if (workload.name === 'small_tx_1000_commits') {
            await smallTxCommits(db, entries);
        } else {
            await bulkInsertPrepared(
                db,
                entries,
                isSingleTransactionInsertWorkload(workload.name) || workload.name === 'indexeddb_bulk_put_10k'
            );
        }
        return;
    }

    if (isRandomGetWorkload(workload.name)) {
        const prepared = requirePrepared(dbName);
        const db = requirePreparedDb(prepared, dbName);
        const keys = prepared.readKeys;
        if (!keys) {
            throw new Error(`prepared IndexedDB read keys missing for ${dbName}`);
        }
        await randomPointGets(db, keys, isBulkRandomGetWorkload(workload.name));
        return;
    }

    if (isRangeScanWorkload(workload.name)) {
        const prepared = requirePrepared(dbName);
        const db = requirePreparedDb(prepared, dbName);
        await rangeScan(db, workload);
        return;
    }

    if (workload.name === 'cold_open_after_100k') {
        await coldOpenAfterPrepared(dbName, workload);
        return;
    }

    throw new NotApplicableError(`IndexedDB workload not implemented: ${workload.name}`);
}

async function openEmptyDbMeasured(name: string): Promise<void> {
    const prepared = requirePrepared(name);
    prepared.db = await openDb(name, true, false);
}

async function cleanupPrepared(name: string): Promise<void> {
    const prepared = preparedSamples.get(name);
    preparedSamples.delete(name);
    if (!prepared) {
        return;
    }
    prepared.db?.close();
    prepared.db = undefined;
    for (const cleanupName of prepared.cleanupNames ?? [name]) {
        await deleteIndexedDb(cleanupName);
    }
}

function requirePrepared(name: string): PreparedIndexedDbSample {
    const prepared = preparedSamples.get(name);
    if (!prepared) {
        throw new Error(`prepared IndexedDB sample missing for ${name}`);
    }
    return prepared;
}

function requirePreparedDb(prepared: PreparedIndexedDbSample, name: string): IDBDatabase {
    if (!prepared.db) {
        throw new Error(`prepared IndexedDB database missing for ${name}`);
    }
    return prepared.db;
}

function requirePreparedEntries(prepared: PreparedIndexedDbSample, name: string): IdbEntry[][] {
    if (!prepared.entries) {
        throw new Error(`prepared IndexedDB entries missing for ${name}`);
    }
    return prepared.entries;
}

async function openExistingDb(name: string): Promise<IndexedDbHandle> {
    const db = await openDb(name, false);
    return { db, name };
}

function openDb(name: string, create: boolean, ensureStore = true): Promise<IDBDatabase> {
    return new Promise((resolve, reject) => {
        const request = indexedDB.open(name, 1);
        request.onupgradeneeded = () => {
            const db = request.result;
            if (ensureStore && !db.objectStoreNames.contains(STORE_NAME)) {
                db.createObjectStore(STORE_NAME);
            }
        };
        request.onsuccess = () => {
            const db = request.result;
            if (ensureStore && !create && !db.objectStoreNames.contains(STORE_NAME)) {
                db.close();
                reject(new Error(`IndexedDB database ${name} does not contain ${STORE_NAME}`));
                return;
            }
            resolve(db);
        };
        request.onerror = () => reject(request.error ?? new Error(`failed to open IndexedDB ${name}`));
        request.onblocked = () => reject(new Error(`blocked opening IndexedDB ${name}`));
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

function effectiveBatchSize(workload: WorkloadSpec): number {
    if (isSingleTransactionInsertWorkload(workload.name)) {
        return Math.min(10_000, workload.recordCount);
    }
    return Math.max(1, workload.batchSize);
}

function shouldUseSingleTransactionPreload(workload: WorkloadSpec): boolean {
    return workload.recordCount >= 1_000_000;
}

function buildEntryBatches(workload: WorkloadSpec, count: number, batchSize: number): IdbEntry[][] {
    const batches: IdbEntry[][] = [];
    for (let start = 0; start < count; start += batchSize) {
        const end = Math.min(start + batchSize, count);
        const entries: IdbEntry[] = [];
        for (let i = start; i < end; i += 1) {
            entries.push([keyString(i, workload.keySize), valueBytes(i, workload.valueSize)]);
        }
        batches.push(entries);
    }
    return batches;
}

function buildRandomReadKeys(workload: WorkloadSpec, sampleIndex: number): string[] {
    const rng = new DeterministicRng(0x1db50000 ^ sampleIndex ^ workload.recordCount);
    const readCount = randomReadCount(workload);
    const keys: string[] = [];
    for (let i = 0; i < readCount; i += 1) {
        keys.push(keyString(rng.nextInt(workload.recordCount), workload.keySize));
    }
    return keys;
}

async function bulkInsertPrepared(db: IDBDatabase, batches: IdbEntry[][], singleTransaction: boolean): Promise<void> {
    if (singleTransaction) {
        await putBatchesInOneTransaction(db, batches);
        return;
    }
    for (const batch of batches) {
        await putBatch(db, batch);
    }
}

function putBatchesInOneTransaction(db: IDBDatabase, batches: IdbEntry[][]): Promise<void> {
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, 'readwrite');
        const store = tx.objectStore(STORE_NAME);
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error ?? new Error('IndexedDB write transaction failed'));
        tx.onabort = () => reject(tx.error ?? new Error('IndexedDB write transaction aborted'));
        for (const batch of batches) {
            for (const [key, value] of batch) {
                store.put(value, key);
            }
        }
    });
}

function putBatch(db: IDBDatabase, entries: IdbEntry[]): Promise<void> {
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, 'readwrite');
        const store = tx.objectStore(STORE_NAME);
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error ?? new Error('IndexedDB write transaction failed'));
        tx.onabort = () => reject(tx.error ?? new Error('IndexedDB write transaction aborted'));
        for (const [key, value] of entries) {
            store.put(value, key);
        }
    });
}

async function smallTxCommits(db: IDBDatabase, batches: IdbEntry[][]): Promise<void> {
    for (const batch of batches) {
        for (const entry of batch) {
            await putBatch(db, [entry]);
        }
    }
}

function randomPointGets(db: IDBDatabase, keys: string[], bulk: boolean): Promise<void> {
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, 'readonly');
        const store = tx.objectStore(STORE_NAME);
        let seen = 0;
        tx.oncomplete = () => {
            if (seen !== keys.length) {
                reject(new Error(`IndexedDB read count mismatch: ${seen} != ${keys.length}`));
                return;
            }
            resolve();
        };
        tx.onerror = () => reject(tx.error ?? new Error('IndexedDB read transaction failed'));
        tx.onabort = () => reject(tx.error ?? new Error('IndexedDB read transaction aborted'));
        for (const key of keys) {
            const request = store.get(key);
            request.onsuccess = () => {
                if (request.result instanceof Uint8Array) {
                    seen += 1;
                }
            };
        }
        void bulk;
    });
}

function rangeScan(db: IDBDatabase, workload: WorkloadSpec): Promise<void> {
    const { start, count } = scanWindow(workload);
    const lower = keyString(start, workload.keySize);
    const upper = keyString(start + count - 1, workload.keySize);
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, 'readonly');
        const store = tx.objectStore(STORE_NAME);
        const request = store.openCursor(IDBKeyRange.bound(lower, upper));
        let rows = 0;
        request.onsuccess = () => {
            const cursor = request.result;
            if (!cursor) {
                return;
            }
            rows += 1;
            cursor.continue();
        };
        tx.oncomplete = () => {
            if (rows !== count) {
                reject(new Error(`IndexedDB range scan count mismatch: ${rows} != ${count}`));
                return;
            }
            resolve();
        };
        tx.onerror = () => reject(tx.error ?? new Error('IndexedDB range transaction failed'));
        tx.onabort = () => reject(tx.error ?? new Error('IndexedDB range transaction aborted'));
    });
}

async function coldOpenAfterPrepared(dbName: string, workload: WorkloadSpec): Promise<void> {
    const prepared = requirePrepared(dbName);
    const reopened = await openExistingDb(dbName);
    prepared.db = reopened.db;
    const value = await readOne(reopened.db, workload, Math.floor(workload.recordCount / 2));
    if (!value) {
        throw new Error('IndexedDB cold-open verification read returned no value');
    }
}

function readOne(db: IDBDatabase, workload: WorkloadSpec, index: number): Promise<Uint8Array | null> {
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, 'readonly');
        const request = tx.objectStore(STORE_NAME).get(keyString(index, workload.keySize));
        request.onsuccess = () => resolve(request.result instanceof Uint8Array ? request.result : null);
        request.onerror = () => reject(request.error ?? new Error('IndexedDB read failed'));
        tx.onerror = () => reject(tx.error ?? new Error('IndexedDB read transaction failed'));
        tx.onabort = () => reject(tx.error ?? new Error('IndexedDB read transaction aborted'));
    });
}
