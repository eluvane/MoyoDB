import { openDB, type DB } from '../src/index';
import type { SampleContext, WorkloadRunner, WorkloadSpec } from './types';
import {
    DeterministicRng,
    isBulkInsertWorkload,
    isBulkRandomGetWorkload,
    isRandomGetWorkload,
    isRangeScanWorkload,
    isSdkPutSingleCallsWorkload,
    isSingleTransactionInsertWorkload,
    keyBytes,
    OPFS_DIAGNOSTIC_BYTES,
    randomReadCount,
    scanWindow,
    STORE_NAME,
    valueBytes
} from './workloads';

type Entry = [Uint8Array, Uint8Array];

type PreparedMoyoSample = {
    db?: DB;
    entries?: Entry[][];
    readKeys?: Uint8Array[];
    worker?: Worker;
    wasmDiagnosticDb?: string;
    rawOpfsFiles?: string[];
    cleanupNames?: string[];
    roundtripPayloads?: Uint8Array[];
};

export const moyoDbBaseline: WorkloadRunner = {
    engine: 'moyodb',
    async prepare(ctx: SampleContext): Promise<(() => Promise<void>) | void> {
        await requireMoyoDbCapabilitiesForWorkload(ctx.workload);
        const prepared: PreparedMoyoSample = { cleanupNames: [ctx.dbName] };

        if (isWorkerIpcDiagnosticWorkload(ctx.workload.name)) {
            prepared.worker = createEchoWorker();
            if (ctx.workload.valueSize > 0) {
                prepared.roundtripPayloads = buildRoundtripPayloads(ctx.workload);
            }
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (
            ctx.workload.name === 'noop_wasm_call_100k' ||
            ctx.workload.name === 'engine_bulk_put_10k' ||
            ctx.workload.name === 'engine_stage_put_10k_rollback'
        ) {
            prepared.worker = createWasmDiagnosticWorker();
            prepared.wasmDiagnosticDb = ctx.dbName;
            await postWorker(prepared.worker, {
                op: 'prepareEngine',
                name: ctx.dbName,
                store: STORE_NAME,
                count: ctx.workload.name === 'noop_wasm_call_100k' ? 0 : ctx.workload.recordCount,
                keySize: ctx.workload.keySize,
                valueSize: ctx.workload.valueSize,
                moduleUrl: new URL('/engine/moyodb_engine.js', window.location.href).href,
                wasmUrl: new URL('/engine/moyodb_engine_bg.wasm', window.location.href).href
            });
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (ctx.workload.name === 'opfs_raw_write_100mb') {
            prepared.worker = createOpfsDiagnosticWorker();
            prepared.rawOpfsFiles = [`${ctx.dbName}-raw-write`];
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (ctx.workload.name === 'opfs_raw_read_random_10k') {
            prepared.worker = createOpfsDiagnosticWorker();
            prepared.rawOpfsFiles = [`${ctx.dbName}-raw-read`];
            await postWorker(prepared.worker, {
                op: 'write',
                name: `${ctx.dbName}-raw-read`,
                size: OPFS_DIAGNOSTIC_BYTES,
                chunkSize: ctx.workload.batchSize
            });
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (ctx.workload.name === 'noop_js_loop_1m' || ctx.workload.name === 'encode_decode_10k_256b') {
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (ctx.workload.name === 'open_empty_db') {
            await deleteDBIfExists(ctx.dbName);
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (
            isSdkPutSingleCallsWorkload(ctx.workload.name) ||
            ctx.workload.name === 'sdk_bulk_put_10k' ||
            isBulkInsertWorkload(ctx.workload.name) ||
            ctx.workload.name === 'small_tx_1000_commits'
        ) {
            await deleteDBIfExists(ctx.dbName);
            prepared.db = await openEmptyDb(ctx.dbName);
            await prepared.db.createStore(STORE_NAME);
            prepared.entries = buildEntryBatches(
                ctx.workload,
                ctx.workload.recordCount,
                effectiveBatchSize(ctx.workload)
            );
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (isRandomGetWorkload(ctx.workload.name) || isRangeScanWorkload(ctx.workload.name)) {
            await deleteDBIfExists(ctx.dbName);
            prepared.db = await openEmptyDb(ctx.dbName);
            await prepared.db.createStore(STORE_NAME);
            const entries = buildEntryBatches(ctx.workload, ctx.workload.recordCount, effectiveBatchSize(ctx.workload));
            await bulkInsertPrepared(prepared.db, entries, shouldUseSingleTransactionPreload(ctx.workload));
            if (isRandomGetWorkload(ctx.workload.name)) {
                prepared.readKeys = buildRandomReadKeys(ctx.workload, ctx.sampleIndex);
            }
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (ctx.workload.name === 'cold_open_after_100k') {
            await deleteDBIfExists(ctx.dbName);
            const db = await openEmptyDb(ctx.dbName);
            try {
                await db.createStore(STORE_NAME);
                const entries = buildEntryBatches(
                    ctx.workload,
                    ctx.workload.recordCount,
                    effectiveBatchSize(ctx.workload)
                );
                await bulkInsertPrepared(db, entries, false);
            } finally {
                await db.close().catch(() => undefined);
            }
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (ctx.workload.name === 'worker_roundtrip_overhead') {
            await deleteDBIfExists(ctx.dbName);
            prepared.db = await openEmptyDb(ctx.dbName);
            await prepared.db.createStore(STORE_NAME);
            await prepared.db.put(STORE_NAME, keyBytes(0, ctx.workload.keySize), valueBytes(0, ctx.workload.valueSize));
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        if (ctx.workload.name === 'snapshot_export_import') {
            const sourceName = `${ctx.dbName}-source`;
            const targetName = `${ctx.dbName}-target`;
            prepared.cleanupNames = [sourceName, targetName];
            await deleteDBIfExists(sourceName);
            await deleteDBIfExists(targetName);
            prepared.db = await openEmptyDb(sourceName);
            await prepared.db.createStore(STORE_NAME);
            prepared.entries = buildEntryBatches(
                ctx.workload,
                ctx.workload.recordCount,
                effectiveBatchSize(ctx.workload)
            );
            await bulkInsertPrepared(prepared.db, prepared.entries, false);
            preparedSamples.set(ctx.dbName, prepared);
            return () => cleanupPrepared(ctx.dbName);
        }

        return undefined;
    },
    async run(ctx: SampleContext): Promise<void> {
        if (!ctx.workload.supports.includes('moyodb')) {
            throw new NotApplicableError(`MoyoDB baseline is not applicable to ${ctx.workload.name}`);
        }
        await requireMoyoDbCapabilitiesForWorkload(ctx.workload);
        await runMoyoDbWorkload(ctx.dbName, ctx.workload, ctx.sampleIndex);
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

const preparedSamples = new Map<string, PreparedMoyoSample>();
let capabilityProbe: Promise<void> | null = null;

async function requireMoyoDbCapabilitiesForWorkload(workload: WorkloadSpec): Promise<void> {
    if (
        workload.name === 'noop_js_loop_1m' ||
        workload.name === 'encode_decode_10k_256b' ||
        isWorkerIpcDiagnosticWorkload(workload.name)
    ) {
        if (isWorkerIpcDiagnosticWorkload(workload.name) && typeof globalThis.Worker !== 'function') {
            throw new NotApplicableError('Worker is unavailable.');
        }
        return;
    }
    capabilityProbe ??= probeMoyoDbCapabilities();
    return capabilityProbe;
}

async function probeMoyoDbCapabilities(): Promise<void> {
    const nav = navigator as Navigator & {
        locks?: unknown;
        storage?: StorageManager & { getDirectory?: unknown };
    };
    if (!globalThis.isSecureContext) {
        throw new NotApplicableError('MoyoDB requires a secure browser context.');
    }
    if (!nav.storage || typeof nav.storage.getDirectory !== 'function') {
        throw new NotApplicableError('MoyoDB requires navigator.storage.getDirectory.');
    }
    if (!nav.locks) {
        throw new NotApplicableError('MoyoDB requires navigator.locks.');
    }
    if (typeof globalThis.BroadcastChannel === 'undefined') {
        throw new NotApplicableError('MoyoDB requires BroadcastChannel.');
    }
    if (typeof globalThis.Worker !== 'function') {
        throw new NotApplicableError('MoyoDB requires Worker.');
    }
    if (!(await hasSyncAccessHandle())) {
        throw new NotApplicableError(
            'MoyoDB requires OPFS createSyncAccessHandle in a dedicated worker. This Playwright/browser runtime does not expose that API.'
        );
    }
}

async function hasSyncAccessHandle(): Promise<boolean> {
    try {
        const source = `
self.onmessage = async () => {
  try {
    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle('__moyodb_bench_support__', { create: true });
    const file = await dir.getFileHandle('probe.bin', { create: true });
    if (typeof file.createSyncAccessHandle !== 'function') {
      self.postMessage(false);
      return;
    }
    const handle = await file.createSyncAccessHandle();
    handle.close();
    self.postMessage(true);
  } catch {
    self.postMessage(false);
  }
};`;
        const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
        const worker = new Worker(url, { type: 'module' });
        try {
            return await new Promise<boolean>((resolve) => {
                const timeout = setTimeout(() => resolve(false), 3000);
                worker.onmessage = (event) => {
                    clearTimeout(timeout);
                    resolve(event.data === true);
                };
                worker.onerror = () => {
                    clearTimeout(timeout);
                    resolve(false);
                };
                worker.postMessage(null);
            });
        } finally {
            worker.terminate();
            URL.revokeObjectURL(url);
        }
    } catch {
        return false;
    }
}

async function runMoyoDbWorkload(dbName: string, workload: WorkloadSpec, sampleIndex: number): Promise<void> {
    switch (workload.name) {
        case 'noop_js_loop_1m':
            jsNoopLoop(workload.recordCount);
            return;
        case 'noop_worker_roundtrip_10k':
        case 'worker_roundtrip_noop':
            await workerRoundtripNoop(dbName, workload.recordCount);
            return;
        case 'worker_roundtrip_small_payload':
        case 'worker_roundtrip_256b_payload':
        case 'worker_roundtrip_64kb_payload':
            await workerRoundtripPayload(dbName, workload, false);
            return;
        case 'worker_binary_transfer_64kb':
            await workerRoundtripPayload(dbName, workload, true);
            return;
        case 'noop_wasm_call_100k':
            await noopWasmCalls(dbName, workload.recordCount);
            return;
        case 'encode_decode_10k_256b':
            encodeDecodeDiagnostic(workload);
            return;
        case 'opfs_raw_write_100mb':
            await opfsRawWrite(dbName, workload);
            return;
        case 'opfs_raw_read_random_10k':
            await opfsRawReadRandom(dbName, workload, sampleIndex);
            return;
        case 'open_empty_db':
            await openEmptyDbMeasured(dbName);
            return;
        case 'sdk_bulk_put_10k':
            await sdkBulkPut(dbName);
            return;
        case 'engine_stage_put_10k_rollback':
            await engineStagePutRollback(dbName);
            return;
        case 'engine_bulk_put_10k':
            await engineBulkPut(dbName);
            return;
        case 'cold_open_after_100k':
            await coldOpenAfterPrepared(dbName, workload);
            return;
        case 'worker_roundtrip_overhead':
            await workerRoundtripOverhead(dbName, workload);
            return;
        case 'recovery_after_dirty_close':
            await recoveryAfterDirtyClose(dbName, workload);
            return;
        case 'snapshot_export_import':
            await snapshotExportImport(dbName);
            return;
        default:
            break;
    }

    if (isSdkPutSingleCallsWorkload(workload.name)) {
        await sdkPutSingleCalls(dbName);
        return;
    }

    if (isBulkInsertWorkload(workload.name) || workload.name === 'small_tx_1000_commits') {
        const prepared = requirePrepared(dbName);
        const db = requirePreparedDb(prepared, dbName);
        const entries = requirePreparedEntries(prepared, dbName);
        if (workload.name === 'small_tx_1000_commits') {
            await smallTxCommits(db, entries);
        } else {
            await bulkInsertPrepared(db, entries, isSingleTransactionInsertWorkload(workload.name));
        }
        return;
    }

    if (isRandomGetWorkload(workload.name)) {
        const prepared = requirePrepared(dbName);
        const db = requirePreparedDb(prepared, dbName);
        const keys = prepared.readKeys;
        if (!keys) {
            throw new Error(`prepared random read keys missing for ${dbName}`);
        }
        if (isBulkRandomGetWorkload(workload.name)) {
            await randomPointGetsBulk(db, keys);
        } else {
            await randomPointGets(db, keys);
        }
        return;
    }

    if (isRangeScanWorkload(workload.name)) {
        const prepared = requirePrepared(dbName);
        const db = requirePreparedDb(prepared, dbName);
        await rangeScan(db, workload);
        return;
    }

    throw new NotApplicableError(`MoyoDB workload not implemented: ${workload.name}`);
}

async function openEmptyDbMeasured(name: string): Promise<void> {
    const prepared = requirePrepared(name);
    const db = await openEmptyDb(name);
    prepared.db = db;
}

async function freshDb(name: string): Promise<DB> {
    await deleteDBIfExists(name);
    const db = await openEmptyDb(name);
    await db.createStore(STORE_NAME);
    return db;
}

async function openEmptyDb(name: string): Promise<DB> {
    return await openDB(name, { requestPersistence: false });
}

async function cleanupPrepared(name: string): Promise<void> {
    const prepared = preparedSamples.get(name);
    preparedSamples.delete(name);
    if (!prepared) {
        return;
    }
    if (prepared.worker) {
        if (prepared.wasmDiagnosticDb) {
            await postWorker(prepared.worker, { op: 'cleanup', name: prepared.wasmDiagnosticDb }, 5000).catch(
                () => undefined
            );
        }
        for (const rawName of prepared.rawOpfsFiles ?? []) {
            await postWorker(prepared.worker, { op: 'delete', name: rawName }, 5000).catch(() => undefined);
        }
        prepared.worker.terminate();
        prepared.worker = undefined;
    }
    if (prepared.db) {
        await prepared.db.close().catch(() => undefined);
        prepared.db = undefined;
    }
    for (const cleanupName of prepared.cleanupNames ?? [name]) {
        await deleteDBIfExists(cleanupName);
    }
}

async function deleteDBIfExists(name: string): Promise<void> {
    try {
        await removeOpfsDbDirectory(name);
    } catch {
        // Benchmark cleanup is best-effort; open/create will report real failures.
    }
}

async function removeOpfsDbDirectory(name: string): Promise<void> {
    const storage = navigator.storage as StorageManager & {
        getDirectory?: () => Promise<FileSystemDirectoryHandle>;
    };
    if (typeof storage.getDirectory !== 'function') {
        return;
    }
    const root = await storage.getDirectory();
    const stackdb = await root.getDirectoryHandle('stackdb', { create: false }).catch(() => null);
    if (!stackdb) {
        return;
    }
    await stackdb.removeEntry(encodedDbName(name), { recursive: true }).catch(() => undefined);
}

function encodedDbName(value: string): string {
    return Array.from(new TextEncoder().encode(value), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

function requirePrepared(name: string): PreparedMoyoSample {
    const prepared = preparedSamples.get(name);
    if (!prepared) {
        throw new Error(`prepared MoyoDB sample missing for ${name}`);
    }
    return prepared;
}

function requirePreparedDb(prepared: PreparedMoyoSample, name: string): DB {
    if (!prepared.db) {
        throw new Error(`prepared MoyoDB database missing for ${name}`);
    }
    return prepared.db;
}

function requirePreparedEntries(prepared: PreparedMoyoSample, name: string): Entry[][] {
    if (!prepared.entries) {
        throw new Error(`prepared MoyoDB entries missing for ${name}`);
    }
    return prepared.entries;
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

function buildEntryBatches(workload: WorkloadSpec, count: number, batchSize: number): Entry[][] {
    const batches: Entry[][] = [];
    for (let start = 0; start < count; start += batchSize) {
        const end = Math.min(start + batchSize, count);
        const entries: Entry[] = [];
        for (let i = start; i < end; i += 1) {
            entries.push([keyBytes(i, workload.keySize), valueBytes(i, workload.valueSize)]);
        }
        batches.push(entries);
    }
    return batches;
}

function buildRoundtripPayloads(workload: WorkloadSpec): Uint8Array[] {
    const payloads: Uint8Array[] = [];
    for (let i = 0; i < workload.recordCount; i += 1) {
        payloads.push(valueBytes(i, workload.valueSize));
    }
    return payloads;
}

function buildRandomReadKeys(workload: WorkloadSpec, sampleIndex: number): Uint8Array[] {
    const rng = new DeterministicRng(0x0db00000 ^ sampleIndex ^ workload.recordCount);
    const readCount = randomReadCount(workload);
    const keys: Uint8Array[] = [];
    for (let i = 0; i < readCount; i += 1) {
        keys.push(keyBytes(rng.nextInt(workload.recordCount), workload.keySize));
    }
    return keys;
}

async function bulkInsertPrepared(db: DB, batches: Entry[][], singleTransaction: boolean): Promise<void> {
    if (singleTransaction) {
        const tx = await db.begin('readwrite');
        try {
            for (const entries of batches) {
                await tx.putMany(STORE_NAME, entries);
            }
            await tx.commit();
        } catch (error) {
            await tx.rollback().catch(() => undefined);
            throw error;
        }
        return;
    }

    for (const entries of batches) {
        const tx = await db.begin('readwrite');
        try {
            await tx.putMany(STORE_NAME, entries);
            await tx.commit();
        } catch (error) {
            await tx.rollback().catch(() => undefined);
            throw error;
        }
    }
}

async function smallTxCommits(db: DB, batches: Entry[][]): Promise<void> {
    for (const entries of batches) {
        for (const [key, value] of entries) {
            await db.put(STORE_NAME, key, value);
        }
    }
}

async function sdkPutSingleCalls(dbName: string): Promise<void> {
    const prepared = requirePrepared(dbName);
    const db = requirePreparedDb(prepared, dbName);
    const entries = requirePreparedEntries(prepared, dbName);
    for (const batch of entries) {
        for (const [key, value] of batch) {
            await db.put(STORE_NAME, key, value);
        }
    }
}

async function sdkBulkPut(dbName: string): Promise<void> {
    const prepared = requirePrepared(dbName);
    const db = requirePreparedDb(prepared, dbName);
    const entries = requirePreparedEntries(prepared, dbName).flat();
    const tx = await db.begin('readwrite');
    try {
        await tx.putMany(STORE_NAME, entries);
        await tx.commit();
    } catch (error) {
        await tx.rollback().catch(() => undefined);
        throw error;
    }
}

async function randomPointGets(db: DB, keys: Uint8Array[]): Promise<void> {
    const tx = await db.begin('readonly');
    try {
        for (const key of keys) {
            const value = await tx.get(STORE_NAME, key);
            if (!value) {
                throw new Error('MoyoDB random point read returned no value');
            }
        }
    } finally {
        await tx.rollback();
    }
}

async function randomPointGetsBulk(db: DB, keys: Uint8Array[]): Promise<void> {
    const tx = await db.begin('readonly');
    try {
        const values = await tx.getMany(STORE_NAME, keys);
        if (values.length !== keys.length) {
            throw new Error(`MoyoDB bulk get count mismatch: ${values.length} != ${keys.length}`);
        }
        for (const value of values) {
            if (!value) {
                throw new Error('MoyoDB bulk random point read returned no value');
            }
        }
    } finally {
        await tx.rollback();
    }
}

async function rangeScan(db: DB, workload: WorkloadSpec): Promise<void> {
    const { start, count } = scanWindow(workload);
    const tx = await db.begin('readonly');
    try {
        const rows = await tx.scan(STORE_NAME, {
            gte: keyBytes(start, workload.keySize),
            lte: keyBytes(start + count - 1, workload.keySize)
        });
        if (rows.length !== count) {
            throw new Error(`MoyoDB range scan count mismatch: ${rows.length} != ${count}`);
        }
    } finally {
        await tx.rollback();
    }
}

async function coldOpenAfterPrepared(dbName: string, workload: WorkloadSpec): Promise<void> {
    const prepared = requirePrepared(dbName);
    const reopened = await openDB(dbName, { requestPersistence: false });
    prepared.db = reopened;
    const value = await reopened.get(STORE_NAME, keyBytes(Math.floor(workload.recordCount / 2), workload.keySize));
    if (!value) {
        throw new Error('MoyoDB cold-open verification read returned no value');
    }
}

async function recoveryAfterDirtyClose(dbName: string, workload: WorkloadSpec): Promise<void> {
    const db = await freshDb(dbName);
    try {
        const entries = buildEntryBatches(workload, workload.recordCount, effectiveBatchSize(workload));
        await bulkInsertPrepared(db, entries, false);
        await db.setFailpoint('after_wal_flush');
        try {
            await db.put(
                STORE_NAME,
                keyBytes(workload.recordCount + 1, workload.keySize),
                valueBytes(1, workload.valueSize)
            );
            throw new Error('expected failpoint to abort commit');
        } catch (error) {
            if ((error as Error).name !== 'InjectedFailureError') {
                throw error;
            }
        }
    } finally {
        await db.close().catch(() => undefined);
    }

    const recovered = await openDB(dbName, { requestPersistence: false });
    try {
        const base = await recovered.get(STORE_NAME, keyBytes(0, workload.keySize));
        const after = await recovered.get(STORE_NAME, keyBytes(workload.recordCount + 1, workload.keySize));
        if (!base || !after) {
            throw new Error('MoyoDB recovery verification did not find expected committed values');
        }
    } finally {
        await recovered.close().catch(() => undefined);
        await deleteDBIfExists(dbName);
    }
}

async function snapshotExportImport(dbName: string): Promise<void> {
    const prepared = requirePrepared(dbName);
    const source = requirePreparedDb(prepared, dbName);
    const targetName = `${dbName}-target`;
    const snapshot = await source.exportSnapshot();
    const target = await openDB(targetName, { requestPersistence: false });
    try {
        await target.importSnapshot(snapshot);
        const rows = await target.scan(STORE_NAME, { limit: 1 });
        if (rows.length !== 1) {
            throw new Error('MoyoDB snapshot import verification returned no rows');
        }
    } finally {
        await target.close().catch(() => undefined);
    }
}

async function workerRoundtripOverhead(dbName: string, workload: WorkloadSpec): Promise<void> {
    const prepared = requirePrepared(dbName);
    const db = requirePreparedDb(prepared, dbName);
    for (let i = 0; i < workload.recordCount; i += 1) {
        await db.stats();
    }
}

function jsNoopLoop(count: number): void {
    let acc = 0;
    for (let i = 0; i < count; i += 1) {
        acc = (acc + i) | 0;
    }
    if (acc === Number.MIN_SAFE_INTEGER) {
        throw new Error('unreachable noop guard');
    }
}

function encodeDecodeDiagnostic(workload: WorkloadSpec): void {
    let checksum = 0;
    for (let i = 0; i < workload.recordCount; i += 1) {
        const key = keyBytes(i, workload.keySize);
        const value = valueBytes(i, workload.valueSize);
        checksum ^= key[0] ?? 0;
        checksum ^= value[0] ?? 0;
    }
    if (checksum === Number.MIN_SAFE_INTEGER) {
        throw new Error('unreachable encode/decode guard');
    }
}

function createEchoWorker(): Worker {
    const source = `
self.onmessage = (event) => {
  const data = event.data;
  if (!data || typeof data !== 'object' || !('id' in data)) {
    self.postMessage(data);
    return;
  }
  const payload = data.payload;
  const response = { id: data.id, ok: true, value: payload };
  if (payload && payload.bytes instanceof Uint8Array && payload.transferBack === true) {
    self.postMessage(response, [payload.bytes.buffer]);
    return;
  }
  self.postMessage(response);
};`;
    const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
    const worker = new Worker(url, { type: 'module' });
    URL.revokeObjectURL(url);
    return worker;
}

async function workerRoundtripNoop(dbName: string, count: number): Promise<void> {
    const worker = requirePrepared(dbName).worker;
    if (!worker) {
        throw new Error('prepared echo worker missing');
    }
    for (let i = 0; i < count; i += 1) {
        const response = await postWorker<number>(worker, i);
        if (response !== i) {
            throw new Error('worker echo mismatch');
        }
    }
}

async function workerRoundtripPayload(dbName: string, workload: WorkloadSpec, transfer: boolean): Promise<void> {
    const prepared = requirePrepared(dbName);
    const worker = prepared.worker;
    const payloads = prepared.roundtripPayloads;
    if (!worker || !payloads) {
        throw new Error('prepared worker payload sample missing');
    }
    for (let i = 0; i < payloads.length; i += 1) {
        const bytes = payloads[i];
        const response = transfer
            ? await postWorker<{ bytes: Uint8Array; transferBack: boolean }>(worker, { bytes, transferBack: true }, 0, [
                  bytes.buffer
              ])
            : await postWorker<{ bytes: Uint8Array; transferBack: boolean }>(worker, { bytes, transferBack: false });
        if (!(response.bytes instanceof Uint8Array) || response.bytes.byteLength !== workload.valueSize) {
            throw new Error('worker payload echo mismatch');
        }
    }
}

function createWasmDiagnosticWorker(): Worker {
    const source = `
const STORE = 'kv';
const TEXT_ENCODER = new TextEncoder();
const prepared = new Map();
let wasmReady = null;
let wasmConfig = null;
function keyString(index, keySize) {
  if (keySize <= 0) return '';
  const numeric = Math.max(0, index).toString(16).padStart(12, '0');
  const raw = 'k' + numeric;
  return raw.length >= keySize ? raw.slice(0, keySize) : raw + '_'.repeat(keySize - raw.length);
}
function keyBytes(index, keySize) {
  return TEXT_ENCODER.encode(keyString(index, keySize));
}
function valueBytes(index, valueSize) {
  const value = new Uint8Array(valueSize);
  let state = (index + 1) >>> 0;
  for (let i = 0; i < value.length; i += 1) {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    value[i] = state & 0xff;
  }
  return value;
}
async function loadWasm() {
  if (!wasmReady) {
    if (!wasmConfig) throw new Error('missing WASM diagnostic module URL');
    wasmReady = (async () => {
      const module = await import(wasmConfig.moduleUrl);
      await module.default({ module_or_path: wasmConfig.wasmUrl });
      return module;
    })();
  }
  return await wasmReady;
}
async function prepareEngine(payload) {
  wasmConfig = payload;
  const wasm = await loadWasm();
  try { await wasm.deleteDB(payload.name); } catch {}
  const engine = new wasm.WasmEngine();
  await engine.open(payload.name, { create_if_missing: true, cache_pages: 256 });
  const tx = engine.begin_tx('readwrite');
  engine.create_store(tx, payload.store || STORE, {});
  engine.commit_tx(tx);
  const entries = [];
  for (let i = 0; i < payload.count; i += 1) {
    entries.push([keyBytes(i, payload.keySize), valueBytes(i, payload.valueSize)]);
  }
  prepared.set(payload.name, { wasm, engine, entries, store: payload.store || STORE });
}
function requirePrepared(name) {
  const state = prepared.get(name);
  if (!state) throw new Error('missing prepared WASM diagnostic state for ' + name);
  return state;
}
async function cleanup(name) {
  const state = prepared.get(name);
  prepared.delete(name);
  if (state) {
    try { state.engine.close(); } catch {}
    try { await state.wasm.deleteDB(name); } catch {}
  }
}
self.onmessage = async (event) => {
  const { id, payload } = event.data;
  try {
    if (payload.op === 'prepareEngine') {
      await prepareEngine(payload);
      self.postMessage({ id, ok: true, value: null });
      return;
    }
    if (payload.op === 'noopWasm') {
      const state = requirePrepared(payload.name);
      let checksum = 0;
      for (let i = 0; i < payload.count; i += 1) {
        checksum ^= Number(state.engine.get_schema_version()) & 0xff;
      }
      self.postMessage({ id, ok: true, value: checksum });
      return;
    }
    if (payload.op === 'engineBulkPut') {
      const state = requirePrepared(payload.name);
      const tx = state.engine.begin_tx('readwrite');
      try {
        state.engine.put_many(tx, state.store, state.entries, {});
        state.engine.commit_tx(tx);
      } catch (error) {
        try { state.engine.rollback_tx(tx); } catch {}
        throw error;
      }
      self.postMessage({ id, ok: true, value: state.entries.length });
      return;
    }
    if (payload.op === 'engineStagePutRollback') {
      const state = requirePrepared(payload.name);
      const tx = state.engine.begin_tx('readwrite');
      try {
        state.engine.put_many(tx, state.store, state.entries, {});
      } finally {
        try { state.engine.rollback_tx(tx); } catch {}
      }
      self.postMessage({ id, ok: true, value: state.entries.length });
      return;
    }
    if (payload.op === 'cleanup') {
      await cleanup(payload.name);
      self.postMessage({ id, ok: true, value: null });
      return;
    }
    throw new Error('unknown WASM diagnostic op ' + payload.op);
  } catch (error) {
    self.postMessage({ id, ok: false, error: error instanceof Error ? error.message : String(error) });
  }
};`;
    const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
    const worker = new Worker(url, { type: 'module' });
    URL.revokeObjectURL(url);
    return worker;
}

async function noopWasmCalls(dbName: string, count: number): Promise<void> {
    const worker = requirePrepared(dbName).worker;
    if (!worker) {
        throw new Error('prepared WASM diagnostic worker missing');
    }
    await postWorker(worker, { op: 'noopWasm', name: dbName, count });
}

async function engineBulkPut(dbName: string): Promise<void> {
    const worker = requirePrepared(dbName).worker;
    if (!worker) {
        throw new Error('prepared WASM diagnostic worker missing');
    }
    await postWorker(worker, { op: 'engineBulkPut', name: dbName });
}

async function engineStagePutRollback(dbName: string): Promise<void> {
    const worker = requirePrepared(dbName).worker;
    if (!worker) {
        throw new Error('prepared WASM diagnostic worker missing');
    }
    await postWorker(worker, { op: 'engineStagePutRollback', name: dbName });
}

function createOpfsDiagnosticWorker(): Worker {
    const source = `
function fillChunk(chunk, seed) {
  let state = seed >>> 0;
  for (let i = 0; i < chunk.length; i += 1) {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    chunk[i] = state & 0xff;
  }
}
function nextU32(state) {
  return (Math.imul(state, 1664525) + 1013904223) >>> 0;
}
async function fileHandle(name) {
  const root = await navigator.storage.getDirectory();
  const dir = await root.getDirectoryHandle('__moyodb_bench_raw__', { create: true });
  return await dir.getFileHandle(name + '.bin', { create: true });
}
async function removeFile(name) {
  try {
    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle('__moyodb_bench_raw__', { create: true });
    await dir.removeEntry(name + '.bin');
  } catch {}
}
self.onmessage = async (event) => {
  const { id, payload } = event.data;
  try {
    if (payload.op === 'write') {
      const file = await fileHandle(payload.name);
      if (typeof file.createSyncAccessHandle !== 'function') throw new Error('createSyncAccessHandle unavailable');
      const handle = await file.createSyncAccessHandle();
      const chunk = new Uint8Array(payload.chunkSize);
      let offset = 0;
      while (offset < payload.size) {
        const length = Math.min(chunk.length, payload.size - offset);
        fillChunk(chunk.subarray(0, length), offset ^ payload.size);
        handle.write(chunk.subarray(0, length), { at: offset });
        offset += length;
      }
      handle.flush();
      handle.close();
      self.postMessage({ id, ok: true, value: offset });
      return;
    }
    if (payload.op === 'readRandom') {
      const file = await fileHandle(payload.name);
      const handle = await file.createSyncAccessHandle();
      const chunk = new Uint8Array(payload.valueSize);
      const slots = Math.max(1, Math.floor(payload.size / payload.valueSize));
      let state = payload.seed >>> 0;
      let checksum = 0;
      for (let i = 0; i < payload.readCount; i += 1) {
        state = nextU32(state);
        const slot = state % slots;
        const bytesRead = handle.read(chunk, { at: slot * payload.valueSize });
        checksum ^= bytesRead;
        checksum ^= chunk[0] ?? 0;
      }
      handle.close();
      self.postMessage({ id, ok: true, value: checksum });
      return;
    }
    if (payload.op === 'delete') {
      await removeFile(payload.name);
      self.postMessage({ id, ok: true, value: null });
      return;
    }
    throw new Error('unknown op ' + payload.op);
  } catch (error) {
    self.postMessage({ id, ok: false, error: error instanceof Error ? error.message : String(error) });
  }
};`;
    const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
    const worker = new Worker(url, { type: 'module' });
    URL.revokeObjectURL(url);
    return worker;
}

async function opfsRawWrite(dbName: string, workload: WorkloadSpec): Promise<void> {
    const worker = requirePrepared(dbName).worker;
    if (!worker) {
        throw new Error('prepared OPFS diagnostic worker missing');
    }
    await postWorker(worker, {
        op: 'write',
        name: `${dbName}-raw-write`,
        size: OPFS_DIAGNOSTIC_BYTES,
        chunkSize: workload.batchSize
    });
    await postWorker(worker, { op: 'delete', name: `${dbName}-raw-write` });
}

async function opfsRawReadRandom(dbName: string, workload: WorkloadSpec, sampleIndex: number): Promise<void> {
    const worker = requirePrepared(dbName).worker;
    if (!worker) {
        throw new Error('prepared OPFS diagnostic worker missing');
    }
    await postWorker(worker, {
        op: 'readRandom',
        name: `${dbName}-raw-read`,
        size: OPFS_DIAGNOSTIC_BYTES,
        valueSize: workload.valueSize,
        readCount: workload.recordCount,
        seed: 0x0f5f0000 ^ sampleIndex
    });
}

let nextWorkerRequestId = 1;
function postWorker<T = unknown>(
    worker: Worker,
    payload: unknown,
    timeoutMs = 0,
    transfer: Transferable[] = []
): Promise<T> {
    const id = nextWorkerRequestId;
    nextWorkerRequestId += 1;
    return new Promise<T>((resolve, reject) => {
        let timeout: ReturnType<typeof setTimeout> | undefined;
        const cleanup = () => {
            if (timeout) {
                clearTimeout(timeout);
                timeout = undefined;
            }
            worker.removeEventListener('message', onMessage);
            worker.removeEventListener('error', onError);
        };
        const onMessage = (event: MessageEvent) => {
            if (event.data?.id !== id && typeof payload === 'object') {
                return;
            }
            cleanup();
            if (event.data?.ok === false) {
                reject(new Error(event.data.error ?? 'worker operation failed'));
                return;
            }
            if (event.data?.id === id) {
                resolve(event.data.value as T);
                return;
            }
            resolve(event.data as T);
        };
        const onError = (event: ErrorEvent) => {
            cleanup();
            reject(event.error ?? new Error(event.message));
        };
        worker.addEventListener('message', onMessage);
        worker.addEventListener('error', onError);
        if (timeoutMs > 0) {
            timeout = setTimeout(() => {
                cleanup();
                reject(new Error(`worker operation timed out after ${timeoutMs}ms`));
            }, timeoutMs);
        }
        if (typeof payload === 'object') {
            worker.postMessage({ id, payload }, transfer);
        } else {
            worker.postMessage(payload);
        }
    });
}

function isWorkerIpcDiagnosticWorkload(name: string): boolean {
    return (
        name === 'noop_worker_roundtrip_10k' ||
        name === 'worker_roundtrip_noop' ||
        name === 'worker_roundtrip_small_payload' ||
        name === 'worker_roundtrip_256b_payload' ||
        name === 'worker_roundtrip_64kb_payload' ||
        name === 'worker_binary_transfer_64kb'
    );
}
