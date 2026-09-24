import type { SampleContext, WorkloadRunner, WorkloadSpec } from './types';
import { withTimeout } from '../src/internal';
import {
    assertChecksum,
    ContentChecksum,
    expectedRowsChecksum,
    expectedValuesChecksum,
    isBulkInsertWorkload,
    isPreloadedReadWorkload,
    isRandomGetWorkload,
    isRangeScanWorkload,
    isReverseScanWorkload,
    isSingleTransactionInsertWorkload,
    keyBytes,
    randomReadIndices,
    readRequestMode,
    scanResultIndices,
    scanWindow,
    STORE_NAME,
    valueBytes,
    verificationKind,
    writeVerificationIndices
} from './workloads';

// Keys are the same bytes MoyoDB stores; IndexedDB orders binary keys bytewise,
// so ranges and reverse scans select the same records in both engines.
type IdbKey = Uint8Array<ArrayBuffer>;
type IdbEntry = [IdbKey, Uint8Array];

interface IndexedDbHandle {
    db: IDBDatabase;
    name: string;
}

interface ScanResult {
    keys: IDBValidKey[];
    values: unknown[];
}

type PreparedIndexedDbSample = {
    durability: IDBTransactionDurability;
    db?: IDBDatabase;
    entries?: IdbEntry[][];
    readIndices?: number[];
    readKeys?: IdbKey[];
    readValues?: unknown[];
    scanResult?: ScanResult;
    cleanupNames?: string[];
};

export const indexedDbBaseline: WorkloadRunner = {
    engine: 'indexeddb',
    async prepare(ctx: SampleContext): Promise<(() => Promise<void>) | void> {
        if (!ctx.workload.supports.includes('indexeddb')) {
            return undefined;
        }
        await requireIndexedDbCapabilities();
        const prepared: PreparedIndexedDbSample = {
            durability: ctx.indexedDbDurability,
            cleanupNames: [ctx.dbName]
        };

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

        if (isPreloadedReadWorkload(ctx.workload.name)) {
            await deleteIndexedDb(ctx.dbName);
            prepared.db = await openDb(ctx.dbName, true);
            const entries = buildEntryBatches(ctx.workload, ctx.workload.recordCount, effectiveBatchSize(ctx.workload));
            await bulkInsertPrepared(
                prepared.db,
                entries,
                shouldUseSingleTransactionPreload(ctx.workload),
                prepared.durability
            );
            if (isRandomGetWorkload(ctx.workload.name)) {
                prepared.readIndices = randomReadIndices(ctx.workload, ctx.sampleIndex);
                prepared.readKeys = prepared.readIndices.map((index) => keyBytes(index, ctx.workload.keySize));
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
                await bulkInsertPrepared(db, entries, false, prepared.durability);
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
    async verify(ctx: SampleContext): Promise<string | null> {
        const prepared = preparedSamples.get(ctx.dbName);
        if (!prepared) {
            return null;
        }
        switch (verificationKind(ctx.workload.name)) {
            case 'point-read':
                return assertChecksum(
                    'IndexedDB point read',
                    valuesChecksum(requireValue(prepared.readValues, 'read values', ctx.dbName)),
                    expectedValuesChecksum(ctx.workload, requireValue(prepared.readIndices, 'read indices', ctx.dbName))
                );
            case 'scan':
                return assertChecksum(
                    'IndexedDB scan',
                    rowsChecksum(requireValue(prepared.scanResult, 'scan result', ctx.dbName)),
                    expectedRowsChecksum(ctx.workload, scanResultIndices(ctx.workload))
                );
            case 'write': {
                const indices = writeVerificationIndices(ctx.workload);
                const values = await pipelinedGets(
                    requirePreparedDb(prepared, ctx.dbName),
                    indices.map((index) => keyBytes(index, ctx.workload.keySize))
                );
                return assertChecksum(
                    'IndexedDB write read-back',
                    valuesChecksum(values),
                    expectedValuesChecksum(ctx.workload, indices)
                );
            }
            case 'none':
                return null;
        }
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
        const entries = requireValue(prepared.entries, 'entries', dbName);
        if (workload.name === 'small_tx_1000_commits') {
            await smallTxCommits(db, entries, prepared.durability);
        } else {
            await bulkInsertPrepared(
                db,
                entries,
                isSingleTransactionInsertWorkload(workload.name) || workload.name === 'indexeddb_bulk_put_10k',
                prepared.durability
            );
        }
        return;
    }

    if (isRandomGetWorkload(workload.name)) {
        const prepared = requirePrepared(dbName);
        const db = requirePreparedDb(prepared, dbName);
        const keys = requireValue(prepared.readKeys, 'read keys', dbName);
        const mode = readRequestMode(workload.name);
        if (mode === 'bulk') {
            throw new NotApplicableError('IndexedDB has no multi-key get; see the pipelined row.');
        }
        prepared.readValues = mode === 'sequential' ? await sequentialGets(db, keys) : await pipelinedGets(db, keys);
        return;
    }

    if (isRangeScanWorkload(workload.name)) {
        const prepared = requirePrepared(dbName);
        prepared.scanResult = await rangeScan(requirePreparedDb(prepared, dbName), workload);
        return;
    }

    if (isReverseScanWorkload(workload.name)) {
        const prepared = requirePrepared(dbName);
        prepared.scanResult = await reverseScanFirst(requirePreparedDb(prepared, dbName));
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
    return requireValue(prepared.db, 'database', name);
}

function requireValue<T>(value: T | undefined, what: string, name: string): T {
    if (value === undefined) {
        throw new Error(`prepared IndexedDB ${what} missing for ${name}`);
    }
    return value;
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
            entries.push([keyBytes(i, workload.keySize), valueBytes(i, workload.valueSize)]);
        }
        batches.push(entries);
    }
    return batches;
}

function transactionDone<T>(tx: IDBTransaction, label: string, result: () => T): Promise<T> {
    return new Promise((resolve, reject) => {
        tx.oncomplete = () => {
            try {
                resolve(result());
            } catch (error) {
                reject(error instanceof Error ? error : new Error(String(error)));
            }
        };
        tx.onerror = () => reject(tx.error ?? new Error(`${label} transaction failed`));
        tx.onabort = () => reject(tx.error ?? new Error(`${label} transaction aborted`));
    });
}

function writeTransaction(db: IDBDatabase, durability: IDBTransactionDurability): IDBTransaction {
    return db.transaction(STORE_NAME, 'readwrite', { durability });
}

async function bulkInsertPrepared(
    db: IDBDatabase,
    batches: IdbEntry[][],
    singleTransaction: boolean,
    durability: IDBTransactionDurability
): Promise<void> {
    if (singleTransaction) {
        await putEntries(db, batches.flat(), durability);
        return;
    }
    for (const batch of batches) {
        await putEntries(db, batch, durability);
    }
}

function putEntries(db: IDBDatabase, entries: IdbEntry[], durability: IDBTransactionDurability): Promise<void> {
    const tx = writeTransaction(db, durability);
    const store = tx.objectStore(STORE_NAME);
    for (const [key, value] of entries) {
        store.put(value, key);
    }
    return transactionDone(tx, 'IndexedDB write', () => undefined);
}

async function smallTxCommits(
    db: IDBDatabase,
    batches: IdbEntry[][],
    durability: IDBTransactionDurability
): Promise<void> {
    for (const batch of batches) {
        for (const entry of batch) {
            await putEntries(db, [entry], durability);
        }
    }
}

/** One request outstanding at a time: the next get is issued from the previous success handler. */
function sequentialGets(db: IDBDatabase, keys: IdbKey[]): Promise<unknown[]> {
    const tx = db.transaction(STORE_NAME, 'readonly');
    const store = tx.objectStore(STORE_NAME);
    const values = new Array<unknown>(keys.length);
    let next = 0;
    const issue = (): void => {
        if (next >= keys.length) {
            return;
        }
        const position = next;
        next += 1;
        const request = store.get(keys[position]);
        request.onsuccess = () => {
            values[position] = request.result;
            issue();
        };
    };
    issue();
    return transactionDone(tx, 'IndexedDB sequential read', () => values);
}

function pipelinedGets(db: IDBDatabase, keys: IdbKey[]): Promise<unknown[]> {
    const tx = db.transaction(STORE_NAME, 'readonly');
    const store = tx.objectStore(STORE_NAME);
    const values = new Array<unknown>(keys.length);
    keys.forEach((key, position) => {
        const request = store.get(key);
        request.onsuccess = () => {
            values[position] = request.result;
        };
    });
    return transactionDone(tx, 'IndexedDB pipelined read', () => values);
}

function rangeScan(db: IDBDatabase, workload: WorkloadSpec): Promise<ScanResult> {
    const { start, count } = scanWindow(workload);
    const range = IDBKeyRange.bound(keyBytes(start, workload.keySize), keyBytes(start + count - 1, workload.keySize));
    const tx = db.transaction(STORE_NAME, 'readonly');
    const store = tx.objectStore(STORE_NAME);
    const keysRequest = store.getAllKeys(range);
    const valuesRequest = store.getAll(range);
    return transactionDone(tx, 'IndexedDB range scan', () => ({
        keys: keysRequest.result,
        values: valuesRequest.result
    }));
}

function reverseScanFirst(db: IDBDatabase): Promise<ScanResult> {
    const tx = db.transaction(STORE_NAME, 'readonly');
    const request = tx.objectStore(STORE_NAME).openCursor(null, 'prev');
    const result: ScanResult = { keys: [], values: [] };
    request.onsuccess = () => {
        const cursor = request.result;
        if (cursor) {
            result.keys.push(cursor.key);
            result.values.push(cursor.value);
        }
    };
    return transactionDone(tx, 'IndexedDB reverse scan', () => result);
}

async function coldOpenAfterPrepared(dbName: string, workload: WorkloadSpec): Promise<void> {
    const prepared = requirePrepared(dbName);
    const reopened = await openExistingDb(dbName);
    prepared.db = reopened.db;
    const [value] = await pipelinedGets(reopened.db, [
        keyBytes(Math.floor(workload.recordCount / 2), workload.keySize)
    ]);
    if (!(value instanceof Uint8Array)) {
        throw new Error('IndexedDB cold-open verification read returned no value');
    }
}

function asBytes(value: unknown): Uint8Array | null {
    if (value instanceof Uint8Array) {
        return value;
    }
    if (value instanceof ArrayBuffer) {
        return new Uint8Array(value);
    }
    if (ArrayBuffer.isView(value)) {
        return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    }
    return null;
}

function valuesChecksum(values: readonly unknown[]): ContentChecksum {
    const checksum = new ContentChecksum();
    for (const value of values) {
        checksum.add(asBytes(value));
    }
    return checksum;
}

function rowsChecksum(result: ScanResult): ContentChecksum {
    if (result.keys.length !== result.values.length) {
        throw new Error(`IndexedDB scan returned ${result.keys.length} keys and ${result.values.length} values`);
    }
    const checksum = new ContentChecksum();
    result.keys.forEach((key, index) => {
        checksum.add(asBytes(key));
        checksum.add(asBytes(result.values[index]));
    });
    return checksum;
}
