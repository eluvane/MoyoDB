import { exposeWorkerApi } from './worker-server';
import {
    packedOptionalValues,
    unpackPackedBatchOpKeys,
    unpackPackedBatchOps,
    unpackPackedBinaryList,
    unpackPackedBinaryPairKeys,
    unpackPackedBinaryPairs,
    unpackPackedOptionalValues,
    type PackedBatchOpKey,
    type PackedOptionalValuesV1
} from './worker-protocol';
import type { CommitAppliedEvent, StoreChangeSet } from './change-events';
import {
    compressionFromStoreFlags,
    decodeStoreValueRecord,
    encodeStoreValueRecord,
    wrapSnapshotWithCompression,
    unwrapSnapshotCompression,
    type CompressionOption
} from './compression';
import { prefixRange, utf8Encode } from './codec';
import { isRecord } from './internal';
import {
    INDEX_METADATA_STORE,
    cloneNormalizedIndexDefinitions,
    compareNormalizedIndexDefinitions,
    decodeIndexEntryKey,
    decodeIndexMetadataValue,
    encodeIndexEntryKey,
    encodeIndexMetadataKey,
    encodeIndexMetadataValue,
    extractLogicalIndexKey,
    findIndexDefinition,
    indexKeyExactRange,
    indexRangeToPhysicalRange,
    indexesForStore,
    isInternalStoreName,
    normalizeIndexDefinitions,
    toPublicIndexDefinitions,
    type NormalizedIndexDef
} from './indexing';
import type { AutocommitCommand, IndexScanPage, WorkerApi, WorkerOpenRequest } from './worker-api';
import type {
    BatchOp,
    ChangeFeed,
    ChangeFeedOptions,
    ChangeFeedSettings,
    CompactionResult,
    CreateStoreOptions,
    DbChange,
    DbStats,
    DebugFailpoint,
    EngineHealth,
    ExportSnapshotOptions,
    IndexDef,
    PutOptions,
    Range,
    ScanItem,
    StorageInfo,
    TxMode
} from './types';
type WasmBatchOutcome =
    | {
          kind: 'put';
          baselineExists: boolean;
      }
    | {
          kind: 'delete';
          deleted: boolean;
      };
type WasmChangeRecord = Omit<ChangeFeed['changes'][number], 'txId'> & {
    txId: number | bigint;
};
type WasmChangeFeed = {
    changes: WasmChangeRecord[];
    latestTxId: number | bigint;
};
type WasmDbStats = Omit<
    DbStats,
    | 'db_id'
    | 'catalog_root_page_id'
    | 'next_page_id'
    | 'last_committed_txid'
    | 'last_replayed_wal_offset'
    | 'manifest_len'
    | 'main_len'
    | 'wal_len'
    | 'health'
> & {
    db_id: number | bigint;
    catalog_root_page_id: number | bigint;
    next_page_id: number | bigint;
    last_committed_txid: number | bigint;
    last_replayed_wal_offset: number | bigint;
    manifest_len: number | bigint;
    main_len: number | bigint;
    wal_len: number | bigint;
    health: WasmEngineHealth;
};
type WasmEngineHealth =
    | { state: 'healthy' }
    | { state: 'recoveryRequired'; reason: string; pendingTxid?: number | bigint | null }
    | { state: 'closed' };
type WasmRecoveryReport = {
    lastCommittedTxid: number | bigint;
    pendingTxid?: number | bigint | null;
    pendingCommitted: boolean;
};
type WasmChangeFeedPolicy = {
    enabled: boolean;
    retainTxids?: number | bigint | null;
};
type WasmEngine = {
    open(name: string, options: unknown): Promise<void>;
    openGeneration(name: string, generationName: string, options: unknown): Promise<void>;
    /** Checkpoints when healthy, then releases the files. */
    close(): void;
    /** Releases the files without writing anything. */
    abandon(): void;
    health(): WasmEngineHealth;
    needs_recovery(): boolean;
    recover(): WasmRecoveryReport;
    checkpoint(): void;
    compact_into(target: WasmEngine): WasmU64;
    change_feed_policy(): WasmChangeFeedPolicy;
    set_change_feed_policy(txId: WasmU64, policy: { enabled: boolean; retainTxids: number | null }): void;
    begin_tx(mode: TxMode): WasmU64;
    commit_tx(txId: WasmU64): WasmU64;
    rollback_tx(txId: WasmU64): void;
    create_store(txId: WasmU64, name: string, options?: unknown): void;
    drop_store(txId: WasmU64, name: string): void;
    clear_store(txId: WasmU64, name: string): void;
    get(txId: WasmU64, store: string, key: Uint8Array): Uint8Array | null;
    get_many(txId: WasmU64, store: string, keys: Array<Uint8Array>): Array<Uint8Array | null>;
    /** Packed optional values: u32 count | count x u32 length (u32::MAX = missing) | bytes. */
    get_many_packed(txId: WasmU64, store: string, keys: Uint8Array): Uint8Array;
    /** Key metadata only; the value is never materialized. */
    has(txId: WasmU64, store: string, key: Uint8Array): boolean;
    /** Returns whether a live value existed before the write. */
    put(txId: WasmU64, store: string, key: Uint8Array, value: Uint8Array, options?: unknown): boolean;
    put_many(txId: WasmU64, store: string, entries: Array<[Uint8Array, Uint8Array]>, options?: unknown): boolean[];
    /** One byte per entry: 1 when a live value existed before the write. */
    put_many_packed(txId: WasmU64, store: string, entries: Uint8Array, options?: unknown): Uint8Array;
    delete(txId: WasmU64, store: string, key: Uint8Array): boolean;
    delete_many(txId: WasmU64, store: string, keys: Array<Uint8Array>): boolean[];
    /** One byte per key: 1 when a live value was deleted. */
    delete_many_packed(txId: WasmU64, store: string, keys: Uint8Array): Uint8Array;
    apply_batch(txId: WasmU64, store: string, ops: Array<BatchOp>): WasmBatchOutcome[];
    /** One byte per op: baselineExists for puts, deleted for deletes. */
    apply_batch_packed(txId: WasmU64, store: string, ops: Uint8Array): Uint8Array;
    scan(txId: WasmU64, store: string, range: unknown): unknown;
    changes_since(txId: WasmU64, options?: unknown): unknown;
    get_schema_version(): WasmU64;
    export_snapshot(): Uint8Array;
    list_store_configs(): unknown;
    import_snapshot(data: Uint8Array): WasmU64;
    reset(): WasmU64;
    list_stores(): unknown;
    set_schema_version(txId: WasmU64, version: WasmU64): void;
    stats(): unknown;
    set_failpoint(failpoint: string | null): void;
};
type WasmModule = {
    default: (
        options?: string | URL | Request | { module_or_path?: string | URL | Request | Promise<Response> | Response }
    ) => Promise<unknown>;
    deleteDB(name: string): Promise<void>;
    prepareRebuildTarget(name: string): Promise<{
        generationName: string;
    }>;
    /**
     * Publishes `generationName` as the active generation, but only if the
     * control file still names `expectedCurrent` (`null` = no control file).
     * The written control file is read back and verified before resolving.
     */
    swapActiveGeneration(name: string, generationName: string, expectedCurrent: string | null): Promise<void>;
    /** Active generation name, `null` when there is no control file; rejects on a corrupt one. */
    readActiveGeneration(name: string): Promise<string | null>;
    cleanupInactiveEntries(name: string): Promise<void>;
    dbDirectorySize(name: string): Promise<number>;
    WasmEngine: new () => WasmEngine;
};
type WasmU64 = bigint;
function toWasmU64(value: number | bigint): WasmU64 {
    return BigInt(value);
}
function fromWasmU64(value: number | bigint): number {
    return typeof value === 'bigint' ? Number(value) : value;
}
function fromOptionalWasmU64(value: number | bigint | null | undefined): number | null {
    return value === null || value === undefined ? null : fromWasmU64(value);
}
function toWasmPutOptions(options: PutOptions): unknown {
    return options.ttl === undefined ? options : { ...options, ttl: toWasmU64(options.ttl) };
}
function toWasmRange(range: Range): Range {
    const normalized: Range = {};
    if (range.gt !== undefined) {
        normalized.gt = range.gt;
    }
    if (range.gte !== undefined) {
        normalized.gte = range.gte;
    }
    if (range.lt !== undefined) {
        normalized.lt = range.lt;
    }
    if (range.lte !== undefined) {
        normalized.lte = range.lte;
    }
    if (range.reverse !== undefined) {
        normalized.reverse = range.reverse;
    }
    if (range.limit !== undefined) {
        normalized.limit = range.limit;
    }
    return normalized;
}
interface TrackedKeyChange {
    key: Uint8Array;
    baselineExists: boolean;
    finalKind: DbChange['kind'];
}
interface TrackedStoreChanges {
    touched: boolean;
    /** Latest store-level change; every tracked key change happened after it. */
    storeLevel: 'clear' | 'drop' | null;
    keys: Map<string, TrackedKeyChange>;
}
type TrackedTxnChanges = Map<string, TrackedStoreChanges>;
type MaintenanceOperation = 'compact' | 'rebuild';
const EMPTY_RANGE: Range = {};
const EMPTY_VALUE = new Uint8Array(0);
const MAX_ENGINE_KEY_BYTES = 1024;
const INDEX_SCAN_PAGE_ROWS = 256;
const INDEX_SCAN_RAW_CHUNK_ROWS = 512;
const HEX_BYTE_STRINGS = Array.from({ length: 256 }, (_value, byte) => byte.toString(16).padStart(2, '0'));
function getOrInsert<K, V>(map: Map<K, V>, key: K, create: () => V): V {
    const existing = map.get(key);
    if (existing !== undefined) {
        return existing;
    }
    const created = create();
    map.set(key, created);
    return created;
}
const PERSISTENCE_BRIDGE_INIT = 'moyodb:persistence-bridge:init';
const PERSISTENCE_BRIDGE_REQUEST = 'moyodb:persistence-bridge:request';
const PERSISTENCE_BRIDGE_RESPONSE = 'moyodb:persistence-bridge:response';
type PersistenceBridgeOperation = 'persist' | 'persisted';
interface PersistenceBridgeRequest {
    type: typeof PERSISTENCE_BRIDGE_REQUEST;
    id: number;
    op: PersistenceBridgeOperation;
}
interface PersistenceBridgeResponse {
    type: typeof PERSISTENCE_BRIDGE_RESPONSE;
    id: number;
    granted: boolean;
}
function isPersistenceBridgeResponse(value: unknown): value is PersistenceBridgeResponse {
    return (
        isRecord(value) &&
        value.type === PERSISTENCE_BRIDGE_RESPONSE &&
        typeof value.id === 'number' &&
        typeof value.granted === 'boolean'
    );
}
class MainThreadPersistenceBridge {
    private port: MessagePort | null = null;
    private nextRequestId = 1;
    private pending = new Map<
        number,
        {
            resolve: (value: boolean) => void;
        }
    >();
    private readyResolve: ((port: MessagePort | null) => void) | null = null;
    private ready: Promise<MessagePort | null>;
    constructor() {
        this.ready = new Promise((resolve) => {
            this.readyResolve = resolve;
        });
        self.addEventListener('message', this.handleInitMessage as EventListener);
    }
    async persisted(): Promise<boolean> {
        return this.request('persisted');
    }
    async persist(): Promise<boolean> {
        return this.request('persist');
    }
    close(): void {
        self.removeEventListener('message', this.handleInitMessage as EventListener);
        if (this.readyResolve) {
            this.readyResolve(null);
            this.readyResolve = null;
        }
        if (this.port) {
            this.port.onmessage = null;
            this.port.close();
            this.port = null;
        }
        for (const pending of this.pending.values()) {
            pending.resolve(false);
        }
        this.pending.clear();
    }
    private async request(op: PersistenceBridgeOperation): Promise<boolean> {
        const port = await this.ready;
        if (!port || this.port !== port) {
            return false;
        }
        return new Promise<boolean>((resolve) => {
            const id = this.nextRequestId;
            this.nextRequestId += 1;
            this.pending.set(id, { resolve });
            const request: PersistenceBridgeRequest = {
                type: PERSISTENCE_BRIDGE_REQUEST,
                id,
                op
            };
            try {
                port.postMessage(request);
            } catch {
                this.pending.delete(id);
                resolve(false);
            }
        });
    }
    private handleInitMessage = (event: MessageEvent<unknown>) => {
        const data = event.data as {
            type?: string;
        } | null;
        if (data?.type !== PERSISTENCE_BRIDGE_INIT) {
            return;
        }
        if (event.ports.length === 0) {
            return;
        }
        const port = event.ports[0];
        if (this.port) {
            port.close();
            return;
        }
        this.port = port;
        this.port.onmessage = this.handleResponse;
        this.port.start();
        self.removeEventListener('message', this.handleInitMessage as EventListener);
        this.readyResolve?.(port);
        this.readyResolve = null;
    };
    private handleResponse = (event: MessageEvent<unknown>) => {
        const data = event.data;
        if (!isPersistenceBridgeResponse(data)) {
            return;
        }
        const pending = this.pending.get(data.id);
        if (!pending) {
            return;
        }
        this.pending.delete(data.id);
        pending.resolve(Boolean(data.granted));
    };
}
async function estimateOriginStorage(): Promise<{
    usage: number;
    quota: number;
}> {
    const storage = navigator.storage;
    if (typeof storage.estimate !== 'function') {
        return { usage: 0, quota: 0 };
    }
    const estimate = await storage.estimate();
    return {
        usage: estimate.usage ?? 0,
        quota: estimate.quota ?? 0
    };
}
function bytesToHex(bytes: Uint8Array): string {
    let out = '';
    for (let index = 0; index < bytes.byteLength; index += 1) {
        out += HEX_BYTE_STRINGS[bytes[index]];
    }
    return out;
}
function bytesEqual(left: Uint8Array, right: Uint8Array): boolean {
    if (left.byteLength !== right.byteLength) {
        return false;
    }
    for (let index = 0; index < left.byteLength; index += 1) {
        if (left[index] !== right[index]) {
            return false;
        }
    }
    return true;
}

function normalizeWasmBytes(value: Uint8Array | ArrayBuffer | ArrayLike<number>): Uint8Array {
    if (value instanceof Uint8Array) {
        return value.byteOffset === 0 && value.byteLength === value.buffer.byteLength ? value : value.slice();
    }
    return new Uint8Array(value);
}
function errorName(error: unknown): string | null {
    if (!isRecord(error)) {
        return null;
    }
    const candidate = error.name ?? error.code;
    return typeof candidate === 'string' ? candidate : null;
}
function isNamedError(error: unknown, name: string): boolean {
    return errorName(error) === name;
}
const INDEX_SNAPSHOT_TRAILER_MAGIC = utf8Encode('BDBIDX1');
const indexSnapshotDecoder = new TextDecoder('utf-8', { fatal: true });
function wrapSnapshotWithIndexDefinitions(snapshot: Uint8Array, defs: ReadonlyArray<NormalizedIndexDef>): Uint8Array {
    if (defs.length === 0) {
        return snapshot;
    }
    const manifest = utf8Encode(JSON.stringify(toPublicIndexDefinitions(defs)));
    const lengthBuffer = new ArrayBuffer(4);
    new DataView(lengthBuffer).setUint32(0, manifest.byteLength, true);
    const out = new Uint8Array(snapshot.byteLength + manifest.byteLength + 4 + INDEX_SNAPSHOT_TRAILER_MAGIC.byteLength);
    let offset = 0;
    out.set(snapshot, offset);
    offset += snapshot.byteLength;
    out.set(manifest, offset);
    offset += manifest.byteLength;
    out.set(new Uint8Array(lengthBuffer), offset);
    offset += 4;
    out.set(INDEX_SNAPSHOT_TRAILER_MAGIC, offset);
    return out;
}
function unwrapSnapshotWithIndexDefinitions(data: Uint8Array): {
    snapshot: Uint8Array;
    indexes: NormalizedIndexDef[];
} {
    const magicLength = INDEX_SNAPSHOT_TRAILER_MAGIC.byteLength;
    if (data.byteLength < magicLength + 4) {
        return { snapshot: data, indexes: [] };
    }
    const magicOffset = data.byteLength - magicLength;
    if (!bytesEqual(data.subarray(magicOffset), INDEX_SNAPSHOT_TRAILER_MAGIC)) {
        return { snapshot: data, indexes: [] };
    }
    const lengthOffset = magicOffset - 4;
    if (lengthOffset < 0) {
        return { snapshot: data, indexes: [] };
    }
    const manifestLength = new DataView(data.buffer, data.byteOffset + lengthOffset, 4).getUint32(0, true);
    const manifestOffset = lengthOffset - manifestLength;
    if (manifestOffset < 0) {
        throw remoteError('CorruptionError', 'invalid snapshot index trailer length');
    }
    let parsed: unknown;
    try {
        parsed = JSON.parse(indexSnapshotDecoder.decode(data.subarray(manifestOffset, lengthOffset)));
    } catch (error) {
        throw remoteError('CorruptionError', `invalid snapshot index trailer: ${String(error)}`);
    }
    return {
        snapshot: data.subarray(0, manifestOffset),
        indexes: normalizeIndexDefinitions(parsed)
    };
}
function lifecycleEventChannel(dbName: string): BroadcastChannel {
    return new BroadcastChannel(`db:${dbName}:events`);
}
function broadcastLifecycleEvent(dbName: string, type: 'owner_changed' | 'db_closed' | 'db_deleted') {
    const channel = lifecycleEventChannel(dbName);
    try {
        channel.postMessage({ type, dbName, txid: null });
    } finally {
        channel.close();
    }
}
class OwnershipLease {
    private releaseResolver: (() => void) | null = null;
    private requestPromise: Promise<unknown> | null = null;
    private held = false;
    async acquire(dbName: string, ownerWaitMs: number): Promise<void> {
        const holdPromise = new Promise<void>((resolve) => {
            this.releaseResolver = resolve;
        });
        const lockName = `db:${dbName}:owner`;
        if (ownerWaitMs === 0) {
            let acquiredResolve: ((value: boolean) => void) | null = null;
            const acquiredPromise = new Promise<boolean>((resolve) => {
                acquiredResolve = resolve;
            });
            const request = navigator.locks.request(
                lockName,
                { mode: 'exclusive', ifAvailable: true },
                async (lock) => {
                    if (!lock) {
                        acquiredResolve?.(false);
                        return false;
                    }
                    this.held = true;
                    acquiredResolve?.(true);
                    await holdPromise;
                    return true;
                }
            );
            const acquired = await acquiredPromise;
            if (!acquired) {
                throw remoteError('DatabaseBusyError', `database ${dbName} is already owned by another tab`);
            }
            this.requestPromise = request;
            return;
        }
        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), ownerWaitMs);
        let acquiredResolve: (() => void) | null = null;
        const acquiredPromise = new Promise<void>((resolve) => {
            acquiredResolve = resolve;
        });
        this.requestPromise = navigator.locks.request(
            lockName,
            { mode: 'exclusive', signal: controller.signal },
            async (_lock) => {
                clearTimeout(timeout);
                this.held = true;
                acquiredResolve?.();
                await holdPromise;
                return true;
            }
        );
        try {
            await Promise.race([acquiredPromise, this.requestPromise]);
        } catch {
            if (!this.held) {
                throw remoteError('DatabaseBusyError', `timed out waiting for ownership of ${dbName}`);
            }
        }
    }
    async release(): Promise<void> {
        if (!this.held) {
            return;
        }
        this.releaseResolver?.();
        try {
            await this.requestPromise;
        } finally {
            this.releaseResolver = null;
            this.requestPromise = null;
            this.held = false;
        }
    }
}
class DbWorker implements WorkerApi {
    private dbName: string | null = null;
    private engine: WasmEngine | null = null;
    private events: BroadcastChannel | null = null;
    private lease: OwnershipLease | null = null;
    private wasmReady: Promise<WasmModule> | null = null;
    private persistenceBridge = new MainThreadPersistenceBridge();
    private txChanges = new Map<number, TrackedTxnChanges>();
    private txModes = new Map<number, TxMode>();
    /**
     * Index schema each transaction runs against: the committed schema at
     * `begin`, replaced by `reconcileIndexes`. Never mutated in place.
     */
    private txIndexSchemas = new Map<number, NormalizedIndexDef[]>();
    private txIndexSchemaChanged = new Set<number>();
    private txStoreCompression = new Map<number, Map<string, CompressionOption>>();
    private committedIndexes: NormalizedIndexDef[] | null = null;
    private committedStoreCompression: Map<string, CompressionOption> | null = null;
    private openOptions: WorkerOpenRequest['options'] | null = null;
    private maintenanceOperation: MaintenanceOperation | null = null;
    async open(request: WorkerOpenRequest): Promise<void> {
        if (this.engine) {
            return;
        }
        assertSecureContext();
        await assertCapabilities();
        this.dbName = request.dbName;
        this.events = new BroadcastChannel(`db:${request.dbName}:events`);
        this.lease = new OwnershipLease();
        let engine: WasmEngine | null = null;
        try {
            await this.lease.acquire(request.dbName, request.options.ownerWaitMs);
            const wasm = await this.loadWasm();
            engine = new wasm.WasmEngine();
            await engine.open(request.dbName, {
                create_if_missing: request.options.createIfMissing,
                cache_pages: request.options.cachePages
            });
            if (request.options.changeFeed) {
                applyChangeFeedPolicy(engine, request.options.changeFeed);
            }
            if (request.options.debugFailpoint) {
                engine.set_failpoint(request.options.debugFailpoint);
            }
            this.engine = engine;
            this.openOptions = request.options;
            this.clearRuntimeCaches();
            this.maintenanceOperation = null;
            this.events.postMessage({ type: 'owner_changed', dbName: request.dbName, txid: null });
        } catch (err) {
            await this.cleanupAfterFailedOpen(engine);
            throw remapError(err);
        }
    }
    async close(): Promise<void> {
        this.ensureNotInMaintenance();
        const dbName = this.dbName;
        this.clearRuntimeCaches();
        try {
            this.engine?.close();
        } catch (err) {
            throw remapError(err);
        } finally {
            this.engine = null;
            if (dbName && this.events) {
                this.events.postMessage({ type: 'db_closed', dbName, txid: null });
            }
            this.events?.close();
            this.events = null;
            if (dbName && this.lease) {
                await this.lease.release();
            }
            if (dbName) {
                broadcastLifecycleEvent(dbName, 'owner_changed');
            }
            this.disposePersistenceBridge();
            this.lease = null;
            this.dbName = null;
            this.openOptions = null;
            this.maintenanceOperation = null;
        }
    }
    async destroy(): Promise<void> {
        this.ensureNotInMaintenance();
        const dbName = this.dbName;
        if (!dbName) {
            return;
        }
        let failure: unknown = null;
        try {
            this.rollbackAllTransactions();
            // The files are deleted next; checkpointing into them is wasted work.
            this.engine?.abandon();
            const wasm = await this.loadWasm();
            await wasm.deleteDB(dbName);
            broadcastLifecycleEvent(dbName, 'db_deleted');
        } catch (err) {
            failure = err;
        } finally {
            this.engine = null;
            this.clearRuntimeCaches();
            this.events?.close();
            this.events = null;
            if (this.lease) {
                try {
                    await this.lease.release();
                } catch (err) {
                    if (failure === null) {
                        failure = err;
                    }
                }
            }
            broadcastLifecycleEvent(dbName, 'owner_changed');
            this.disposePersistenceBridge();
            this.lease = null;
            this.dbName = null;
            this.openOptions = null;
            this.maintenanceOperation = null;
        }
        if (failure !== null) {
            throw remapError(failure);
        }
    }
    async deleteDB(dbName: string): Promise<void> {
        assertSecureContext();
        await assertCapabilities();
        const lease = new OwnershipLease();
        let failure: unknown = null;
        let acquired = false;
        try {
            await lease.acquire(dbName, 0);
            acquired = true;
            const wasm = await this.loadWasm();
            await wasm.deleteDB(dbName);
            broadcastLifecycleEvent(dbName, 'db_deleted');
        } catch (err) {
            failure = err;
        } finally {
            try {
                await lease.release();
            } catch (err) {
                if (failure === null) {
                    failure = err;
                }
            }
            if (acquired) {
                broadcastLifecycleEvent(dbName, 'owner_changed');
            }
        }
        if (failure !== null) {
            throw remapError(failure);
        }
    }
    begin(mode: TxMode): Promise<number> {
        this.recoverEngineIfNeeded();
        const indexSchema = this.loadCommittedIndexSchema();
        const storeCompression = new Map(this.loadCommittedStoreCompression());
        const txId = fromWasmU64(this.withEngine((engine) => engine.begin_tx(mode)));
        this.txChanges.set(txId, new Map<string, TrackedStoreChanges>());
        this.txModes.set(txId, mode);
        this.txIndexSchemas.set(txId, indexSchema);
        this.txStoreCompression.set(txId, storeCompression);
        return Promise.resolve(txId);
    }
    commit(txId: number): Promise<number> {
        const tracked = this.txChanges.get(txId) ?? new Map<string, TrackedStoreChanges>();
        const indexOverride = this.txIndexSchemaChanged.has(txId) ? this.txIndexSchemas.get(txId) : undefined;
        const storeCompressionSnapshot = this.txStoreCompression.get(txId);
        const txMode = this.txModes.get(txId);
        let txid: number;
        try {
            txid = fromWasmU64(this.withEngine((engine) => engine.commit_tx(toWasmU64(txId))));
        } catch (err) {
            this.cleanupTxState(txId);
            const recovery = this.recoverEngineIfNeeded();
            // A durable commit is reported as committed. Injected failpoints
            // stand for a crash, which never gets to report anything.
            if (
                !recovery?.pendingCommitted ||
                recovery.pendingTxid === null ||
                isNamedError(err, 'InjectedFailureError')
            ) {
                throw err;
            }
            txid = recovery.pendingTxid;
        }
        if (indexOverride) {
            this.committedIndexes = cloneNormalizedIndexDefinitions(indexOverride);
        }
        if (txMode === 'readwrite' && storeCompressionSnapshot) {
            this.committedStoreCompression = new Map(storeCompressionSnapshot);
        }
        this.cleanupTxState(txId);
        if (this.dbName && this.events) {
            const event: CommitAppliedEvent = {
                type: 'commit_applied',
                dbName: this.dbName,
                txid,
                stores: this.finalizeTrackedChanges(tracked)
            };
            this.events.postMessage(event);
        }
        return Promise.resolve(txid);
    }
    rollback(txId: number): Promise<void> {
        try {
            this.withEngine((engine) => engine.rollback_tx(toWasmU64(txId)));
        } finally {
            this.cleanupTxState(txId);
        }
        return Promise.resolve();
    }
    async autocommit(mode: TxMode, command: AutocommitCommand, args: unknown[]): Promise<unknown> {
        const txId = await this.begin(mode);
        const method = this[command] as unknown as (...methodArgs: unknown[]) => Promise<unknown>;
        let result: unknown;
        try {
            result = await method.apply(this, [txId, ...args]);
        } catch (error) {
            try {
                await this.rollback(txId);
            } catch {}
            throw error;
        }
        if (mode === 'readwrite') {
            await this.commit(txId);
        } else {
            await this.rollback(txId);
        }
        return result;
    }
    createStore(txId: number, name: string, options: CreateStoreOptions = {}): Promise<void> {
        const compression = options.compression ?? false;
        this.withEngine((engine) => engine.create_store(toWasmU64(txId), name, { compression }));
        this.recordStoreCompressionCreate(txId, name, compression);
        this.ensureInternalStoresForStore(txId, name);
        this.markStoreTouched(txId, name);
        return Promise.resolve();
    }
    dropStore(txId: number, name: string): Promise<void> {
        const defs = this.indexesForStoreInTx(txId, name);
        this.withEngine((engine) => engine.drop_store(toWasmU64(txId), name));
        this.recordStoreCompressionDrop(txId, name);
        for (const def of defs) {
            this.dropRawStoreIfExists(txId, def.internalStore);
        }
        this.recordStoreLevelChange(txId, name, 'drop');
        return Promise.resolve();
    }
    clearStore(txId: number, name: string): Promise<void> {
        const defs = this.indexesForStoreInTx(txId, name);
        this.withEngine((engine) => engine.clear_store(toWasmU64(txId), name));
        for (const def of defs) {
            this.clearRawStoreIfExists(txId, def.internalStore);
        }
        this.recordStoreLevelChange(txId, name, 'clear');
        return Promise.resolve();
    }
    async get(txId: number, store: string, key: Uint8Array): Promise<Uint8Array | null> {
        return this.readStoreValue(txId, store, key);
    }
    async getMany(txId: number, store: string, keys: Array<Uint8Array>): Promise<Array<Uint8Array | null>> {
        const values = this.withEngine((engine) => engine.get_many(toWasmU64(txId), store, keys));
        if (isInternalStoreName(store) || this.storeCompressionForTx(txId, store) === false) {
            return values.map((value) => (value === null ? null : normalizeWasmBytes(value)));
        }
        return Promise.all(
            values.map(async (value) =>
                value === null ? null : this.decodeStoreValue(txId, store, normalizeWasmBytes(value))
            )
        );
    }
    async getManyPacked(
        txId: number,
        store: string,
        packedKeys: Uint8Array
    ): Promise<PackedOptionalValuesV1 | Array<Uint8Array | null>> {
        const packed = normalizeWasmBytes(
            this.withEngine((engine) => engine.get_many_packed(toWasmU64(txId), store, packedKeys))
        );
        if (isInternalStoreName(store) || this.storeCompressionForTx(txId, store) === false) {
            // Raw values go back to the caller in the engine's own layout.
            return packedOptionalValues(packed);
        }
        return Promise.all(
            unpackPackedOptionalValues(packed).map(async (value) =>
                value === null ? null : this.decodeStoreValue(txId, store, value)
            )
        );
    }
    has(txId: number, store: string, key: Uint8Array): Promise<boolean> {
        return Promise.resolve(this.withEngine((engine) => engine.has(toWasmU64(txId), store, key)));
    }
    async put(
        txId: number,
        store: string,
        key: Uint8Array,
        value: Uint8Array,
        options: PutOptions = {}
    ): Promise<void> {
        const defs = this.indexesForStoreInTx(txId, store);
        if (defs.length === 0) {
            const storedValue = await this.encodeStoreValueForWrite(txId, store, value);
            const baselineExists = this.withEngine((engine) =>
                engine.put(toWasmU64(txId), store, key, storedValue, toWasmPutOptions(options))
            );
            this.recordPutChange(txId, store, key, baselineExists);
            return;
        }
        // Index maintenance needs the previous value to find its old index keys.
        const oldValue = await this.readStoreValue(txId, store, key);
        const plans = defs.map((def) => {
            const oldLogicalKey = oldValue === null ? null : extractLogicalIndexKey(def, oldValue);
            const newLogicalKey = extractLogicalIndexKey(def, value);
            return {
                def,
                oldLogicalKey,
                newLogicalKey,
                newPhysicalKey: null as Uint8Array | null
            };
        });
        this.ensureInternalStoresForDefinitions(txId, defs);
        for (const plan of plans) {
            if (plan.newLogicalKey === null) {
                continue;
            }
            plan.newPhysicalKey = this.buildIndexEntryKey(plan.def, plan.newLogicalKey, key);
            if (plan.oldLogicalKey !== null && bytesEqual(plan.oldLogicalKey, plan.newLogicalKey)) {
                continue;
            }
            await this.assertUniqueIndexAvailability(txId, plan.def, plan.newLogicalKey, key);
        }
        const storedValue = await this.encodeStoreValueForWrite(txId, store, value);
        const baselineExists = this.withEngine((engine) =>
            engine.put(toWasmU64(txId), store, key, storedValue, toWasmPutOptions(options))
        );
        for (const plan of plans) {
            if (plan.oldLogicalKey === null) {
                continue;
            }
            if (plan.newLogicalKey !== null && bytesEqual(plan.oldLogicalKey, plan.newLogicalKey)) {
                continue;
            }
            this.deleteIndexEntryIfPresent(txId, plan.def, plan.oldLogicalKey, key);
        }
        for (const plan of plans) {
            if (plan.newLogicalKey === null) {
                continue;
            }
            if (plan.oldLogicalKey !== null && bytesEqual(plan.oldLogicalKey, plan.newLogicalKey)) {
                continue;
            }
            if (plan.newPhysicalKey === null) {
                throw remoteError('InternalError', 'missing index entry key');
            }
            this.putIndexEntry(txId, plan.def, plan.newPhysicalKey, EMPTY_VALUE);
        }
        this.recordPutChange(txId, store, key, baselineExists);
    }
    async putMany(
        txId: number,
        store: string,
        entries: Array<[Uint8Array, Uint8Array]>,
        options: PutOptions = {}
    ): Promise<void> {
        if (entries.length === 0) {
            return;
        }
        const defs = this.indexesForStoreInTx(txId, store);
        if (defs.length === 0) {
            const storedEntries = await this.encodeEntriesForWrite(txId, store, entries);
            let baselines: boolean[];
            try {
                baselines = this.withEngine((engine) =>
                    engine.put_many(toWasmU64(txId), store, storedEntries, toWasmPutOptions(options))
                );
            } catch (error) {
                const partial = partialBooleanOutcomes(error);
                this.recordPutChanges(txId, store, entries, partial);
                throw error;
            }
            if (baselines.length !== entries.length) {
                throw remoteError(
                    'InternalError',
                    `put_many outcome count mismatch: ${baselines.length} != ${entries.length}`
                );
            }
            this.recordPutChanges(txId, store, entries, baselines);
            return;
        }
        for (const [key, value] of entries) {
            await this.put(txId, store, key, value, options);
        }
    }
    async putManyPacked(
        txId: number,
        store: string,
        packedEntries: Uint8Array,
        options: PutOptions = {}
    ): Promise<void> {
        const defs = this.indexesForStoreInTx(txId, store);
        if (defs.length !== 0 || this.storeCompressionForTx(txId, store) !== false) {
            await this.putMany(txId, store, unpackPackedBinaryPairs(packedEntries), options);
            return;
        }
        const keys = unpackPackedBinaryPairKeys(packedEntries);
        if (keys.length === 0) {
            return;
        }
        let baselines: Uint8Array;
        try {
            baselines = this.withEngine((engine) =>
                engine.put_many_packed(toWasmU64(txId), store, packedEntries, toWasmPutOptions(options))
            );
        } catch (error) {
            const partial = partialBooleanOutcomes(error);
            this.recordPutKeyChanges(txId, store, keys, partial);
            throw error;
        }
        if (baselines.length !== keys.length) {
            throw remoteError(
                'InternalError',
                `put_many_packed outcome count mismatch: ${baselines.length} != ${keys.length}`
            );
        }
        this.recordPutKeyChanges(txId, store, keys, baselines);
    }
    async delete(txId: number, store: string, key: Uint8Array): Promise<boolean> {
        const defs = this.indexesForStoreInTx(txId, store);
        // Only index maintenance needs the old value; otherwise the engine's
        // metadata answer is enough.
        const oldValue = defs.length === 0 ? null : await this.readStoreValue(txId, store, key);
        const oldLogicalKeys =
            oldValue === null ? [] : defs.map((def) => ({ def, logicalKey: extractLogicalIndexKey(def, oldValue) }));
        const deleted = this.withEngine((engine) => engine.delete(toWasmU64(txId), store, key));
        if (!deleted) {
            this.recordDeleteChange(txId, store, key, false);
            return false;
        }
        for (const entry of oldLogicalKeys) {
            if (entry.logicalKey !== null) {
                this.deleteIndexEntryIfPresent(txId, entry.def, entry.logicalKey, key);
            }
        }
        this.recordDeleteChange(txId, store, key, true);
        return true;
    }
    async deleteMany(txId: number, store: string, keys: Array<Uint8Array>): Promise<void> {
        if (keys.length === 0) {
            return;
        }
        const defs = this.indexesForStoreInTx(txId, store);
        if (defs.length === 0) {
            let deleted: boolean[];
            try {
                deleted = this.withEngine((engine) => engine.delete_many(toWasmU64(txId), store, keys));
            } catch (error) {
                const partial = partialBooleanOutcomes(error);
                for (let i = 0; i < partial.length && i < keys.length; i += 1) {
                    this.recordDeleteChange(txId, store, keys[i], partial[i]);
                }
                throw error;
            }
            if (deleted.length !== keys.length) {
                throw remoteError(
                    'InternalError',
                    `delete_many outcome count mismatch: ${deleted.length} != ${keys.length}`
                );
            }
            for (let i = 0; i < keys.length; i += 1) {
                this.recordDeleteChange(txId, store, keys[i], deleted[i]);
            }
            return;
        }
        for (const key of keys) {
            await this.delete(txId, store, key);
        }
    }
    async deleteManyPacked(txId: number, store: string, packedKeys: Uint8Array): Promise<void> {
        const defs = this.indexesForStoreInTx(txId, store);
        const keys = unpackPackedBinaryList(packedKeys);
        if (keys.length === 0) {
            return;
        }
        if (defs.length !== 0) {
            await this.deleteMany(txId, store, keys);
            return;
        }
        let deleted: Uint8Array;
        try {
            deleted = this.withEngine((engine) => engine.delete_many_packed(toWasmU64(txId), store, packedKeys));
        } catch (error) {
            const partial = partialBooleanOutcomes(error);
            for (let i = 0; i < partial.length && i < keys.length; i += 1) {
                this.recordDeleteChange(txId, store, keys[i], partial[i]);
            }
            throw error;
        }
        if (deleted.length !== keys.length) {
            throw remoteError(
                'InternalError',
                `delete_many_packed outcome count mismatch: ${deleted.length} != ${keys.length}`
            );
        }
        for (let i = 0; i < keys.length; i += 1) {
            this.recordDeleteChange(txId, store, keys[i], deleted[i] === 1);
        }
    }
    async applyBatch(txId: number, store: string, ops: Array<BatchOp>): Promise<void> {
        if (ops.length === 0) {
            return;
        }
        const defs = this.indexesForStoreInTx(txId, store);
        if (defs.length === 0) {
            const storedOps = await this.encodeBatchOpsForWrite(txId, store, ops);
            let outcomes: WasmBatchOutcome[];
            try {
                outcomes = this.withEngine((engine) => engine.apply_batch(toWasmU64(txId), store, storedOps));
            } catch (error) {
                this.recordBatchOutcomes(txId, store, ops, partialBatchOutcomes(error));
                throw error;
            }
            if (outcomes.length !== ops.length) {
                throw remoteError(
                    'InternalError',
                    `apply_batch outcome count mismatch: ${outcomes.length} != ${ops.length}`
                );
            }
            this.recordBatchOutcomes(txId, store, ops, outcomes);
            return;
        }
        for (const op of ops) {
            if (op.kind === 'put') {
                await this.put(txId, store, op.key, op.value);
            } else {
                await this.delete(txId, store, op.key);
            }
        }
    }
    async applyBatchPacked(txId: number, store: string, packedOps: Uint8Array): Promise<void> {
        const defs = this.indexesForStoreInTx(txId, store);
        if (defs.length !== 0 || this.storeCompressionForTx(txId, store) !== false) {
            const ops = unpackPackedBatchOps(packedOps);
            if (ops.length === 0) {
                return;
            }
            await this.applyBatch(txId, store, ops);
            return;
        }
        const ops = unpackPackedBatchOpKeys(packedOps);
        if (ops.length === 0) {
            return;
        }
        let flags: Uint8Array;
        try {
            flags = this.withEngine((engine) => engine.apply_batch_packed(toWasmU64(txId), store, packedOps));
        } catch (error) {
            this.recordBatchKeyOutcomes(txId, store, ops, partialBatchOutcomes(error));
            throw error;
        }
        if (flags.length !== ops.length) {
            throw remoteError(
                'InternalError',
                `apply_batch_packed outcome count mismatch: ${flags.length} != ${ops.length}`
            );
        }
        for (let i = 0; i < ops.length; i += 1) {
            const op = ops[i];
            if (op.kind === 'put') {
                this.recordPutChange(txId, store, op.key, flags[i] === 1);
            } else {
                this.recordDeleteChange(txId, store, op.key, flags[i] === 1);
            }
        }
    }
    async scan(txId: number, store: string, range: Range): Promise<ScanItem[]> {
        return this.readStoreScan(txId, store, range);
    }
    async getByIndex(txId: number, store: string, indexName: string, key: Uint8Array): Promise<Uint8Array | null> {
        const def = this.resolveIndexDefinition(txId, store, indexName);
        const page = await this.loadVisibleIndexPage(txId, def, prefixRange(key), null, 1);
        return page.rows.length === 0 ? null : page.rows[0].value;
    }
    async scanByIndex(txId: number, store: string, indexName: string, range: Range = {}): Promise<ScanItem[]> {
        const def = this.resolveIndexDefinition(txId, store, indexName);
        const rows: ScanItem[] = [];
        let remaining = range.limit ?? Number.POSITIVE_INFINITY;
        let cursor: Uint8Array | null = null;
        while (remaining > 0) {
            const page = await this.loadVisibleIndexPage(
                txId,
                def,
                range,
                cursor,
                Math.min(remaining, INDEX_SCAN_PAGE_ROWS)
            );
            rows.push(...page.rows);
            remaining -= page.rows.length;
            if (page.cursor === null) {
                break;
            }
            cursor = page.cursor;
        }
        return rows;
    }
    async scanByIndexPage(
        txId: number,
        store: string,
        indexName: string,
        range: Range,
        cursor: Uint8Array | null,
        limit: number
    ): Promise<IndexScanPage> {
        if (!Number.isSafeInteger(limit) || limit <= 0) {
            throw remoteError('InvalidRangeError', 'index scan page limit must be a positive integer');
        }
        const def = this.resolveIndexDefinition(txId, store, indexName);
        return this.loadVisibleIndexPage(txId, def, range, cursor, limit);
    }
    getIndexes(): Promise<IndexDef[]> {
        return Promise.resolve(toPublicIndexDefinitions(this.loadCommittedIndexSchema()));
    }
    async reconcileIndexes(txId: number, indexes: IndexDef[]): Promise<void> {
        const target = normalizeIndexDefinitions(indexes);
        this.ensureRawStore(txId, INDEX_METADATA_STORE);
        const current = this.readIndexSchemaInTx(txId);
        const targetStores = new Set(target.map((def) => def.internalStore));
        for (const def of current) {
            if (!targetStores.has(def.internalStore)) {
                this.dropRawStoreIfExists(txId, def.internalStore);
            }
        }
        this.clearRawStoreIfExists(txId, INDEX_METADATA_STORE);
        for (const def of target) {
            this.ensureRawStore(txId, def.internalStore);
            this.clearRawStoreIfExists(txId, def.internalStore);
            if (this.rawStoreExistsInTx(txId, def.store)) {
                const rows = await this.readStoreScan(txId, def.store, EMPTY_RANGE);
                for (const row of rows) {
                    const logicalKey = extractLogicalIndexKey(def, row.value);
                    if (logicalKey === null) {
                        continue;
                    }
                    await this.assertUniqueIndexAvailability(txId, def, logicalKey, row.key);
                    this.putIndexEntry(txId, def, this.buildIndexEntryKey(def, logicalKey, row.key), EMPTY_VALUE);
                }
            }
            this.putRawValue(
                txId,
                INDEX_METADATA_STORE,
                encodeIndexMetadataKey(def.store, def.name),
                encodeIndexMetadataValue(def)
            );
        }
        this.txIndexSchemas.set(txId, cloneNormalizedIndexDefinitions(target));
        this.txIndexSchemaChanged.add(txId);
    }
    listStores(): Promise<string[]> {
        return Promise.resolve(this.currentStoreNames());
    }
    getVersion(): Promise<number> {
        return Promise.resolve(fromWasmU64(this.withEngine((engine) => engine.get_schema_version())));
    }
    async changesSince(txId: number, options: ChangeFeedOptions = {}): Promise<ChangeFeed> {
        const visibleStores = options.stores?.filter((store) => !isInternalStoreName(store));
        const engineOptions: ChangeFeedOptions = {
            ...options,
            stores: visibleStores ? [...visibleStores] : undefined,
            limit: visibleStores ? options.limit : undefined
        };
        const feed = this.withEngine((engine) =>
            engine.changes_since(toWasmU64(txId), engineOptions)
        ) as WasmChangeFeed;
        const allowedStores = visibleStores ? new Set(visibleStores) : null;
        const changes: ChangeFeed['changes'] = [];
        for (const change of feed.changes) {
            if (isInternalStoreName(change.store)) {
                continue;
            }
            if (allowedStores && !allowedStores.has(change.store)) {
                continue;
            }
            const key = normalizeWasmBytes(change.key);
            let value: Uint8Array | undefined;
            if (change.value !== undefined) {
                value = await this.decodeStoreValueForFeed(change.store, normalizeWasmBytes(change.value));
            }
            changes.push({
                txId: fromWasmU64(change.txId),
                store: change.store,
                key,
                kind: change.kind,
                value
            });
        }
        return {
            latestTxId: fromWasmU64(feed.latestTxId),
            changes: options.limit === undefined ? changes : changes.slice(0, options.limit)
        };
    }
    setSchemaVersion(txId: number, version: number): Promise<void> {
        this.withEngine((engine) => engine.set_schema_version(toWasmU64(txId), toWasmU64(version)));
        return Promise.resolve();
    }
    async exportSnapshot(options: ExportSnapshotOptions = {}): Promise<Uint8Array> {
        const defs = cloneNormalizedIndexDefinitions(this.loadCommittedIndexSchema());
        const bytes = this.withEngine((engine) => engine.export_snapshot());
        const wrapped = wrapSnapshotWithIndexDefinitions(normalizeWasmBytes(bytes), defs);
        return await wrapSnapshotWithCompression(wrapped, options.compression ?? false);
    }
    async importSnapshot(data: Uint8Array): Promise<void> {
        this.rollbackAllTransactions();
        const decompressed = await unwrapSnapshotCompression(data);
        const parsed = unwrapSnapshotWithIndexDefinitions(decompressed);
        this.withEngine((engine) => engine.import_snapshot(parsed.snapshot));
        this.committedIndexes = [];
        this.committedStoreCompression = null;
        await this.applyCommittedIndexSchema(parsed.indexes);
    }
    async reset(): Promise<void> {
        const dbName = this.dbName;
        if (!dbName) {
            throw remoteError('InternalError', 'engine is not open');
        }
        const defs = cloneNormalizedIndexDefinitions(this.loadCommittedIndexSchema());
        this.rollbackAllTransactions();
        const stores: StoreChangeSet[] = this.currentStoreNames().map((store) => ({
            store,
            changes: [{ key: new Uint8Array(0), kind: 'clear' }]
        }));
        const txid = fromWasmU64(this.withEngine((engine) => engine.reset()));
        this.committedIndexes = [];
        await this.applyCommittedIndexSchema(defs);
        if (this.events) {
            const event: CommitAppliedEvent = {
                type: 'commit_applied',
                dbName,
                txid,
                stores
            };
            this.events.postMessage(event);
        }
    }
    async compact(): Promise<CompactionResult> {
        return this.runCompactionOperation('compact');
    }
    async rebuild(): Promise<CompactionResult> {
        return this.runCompactionOperation('rebuild');
    }
    stats(): Promise<DbStats> {
        const stats = this.withEngine((engine) => engine.stats()) as WasmDbStats;
        return Promise.resolve({
            ...stats,
            db_id: fromWasmU64(stats.db_id),
            catalog_root_page_id: fromWasmU64(stats.catalog_root_page_id),
            next_page_id: fromWasmU64(stats.next_page_id),
            last_committed_txid: fromWasmU64(stats.last_committed_txid),
            last_replayed_wal_offset: fromWasmU64(stats.last_replayed_wal_offset),
            manifest_len: fromWasmU64(stats.manifest_len),
            main_len: fromWasmU64(stats.main_len),
            wal_len: fromWasmU64(stats.wal_len),
            health: normalizeEngineHealth(stats.health),
            store_count: this.currentStoreNames().length
        });
    }
    async storageInfo(): Promise<StorageInfo> {
        const [dbSize, estimate, persisted] = await Promise.all([
            this.dbDirectorySize(),
            estimateOriginStorage(),
            this.persistenceBridge.persisted()
        ]);
        return {
            dbSize,
            originUsage: estimate.usage,
            originQuota: estimate.quota,
            persisted
        };
    }
    async requestPersistence(): Promise<boolean> {
        return this.persistenceBridge.persist();
    }
    setFailpoint(failpoint: DebugFailpoint): Promise<void> {
        this.withEngine((engine) => engine.set_failpoint(failpoint));
        return Promise.resolve();
    }
    private requireDbName(): string {
        if (!this.dbName) {
            throw remoteError('InternalError', 'engine is not open');
        }
        return this.dbName;
    }
    private ensureNotInMaintenance(): void {
        if (!this.maintenanceOperation) {
            return;
        }
        const dbName = this.dbName ?? 'database';
        throw remoteError('DatabaseBusyError', `database ${dbName} is busy: ${this.maintenanceOperation} in progress`);
    }
    private currentCachePages(): number {
        return this.openOptions?.cachePages ?? 256;
    }
    private async dbDirectorySize(): Promise<number> {
        const wasm = await this.loadWasm();
        return wasm.dbDirectorySize(this.requireDbName());
    }
    private async applyCommittedIndexSchema(defs: ReadonlyArray<NormalizedIndexDef>): Promise<void> {
        const normalized = cloneNormalizedIndexDefinitions(defs);
        if (normalized.length === 0) {
            this.committedIndexes = [];
            return;
        }
        const txId = fromWasmU64(
            this.withEngine((engine) => engine.begin_tx('readwrite'), { allowDuringMaintenance: true })
        );
        this.txChanges.set(txId, new Map());
        this.txModes.set(txId, 'readwrite');
        this.txStoreCompression.set(txId, new Map(this.loadCommittedStoreCompression()));
        try {
            await this.reconcileIndexes(txId, toPublicIndexDefinitions(normalized));
            this.withEngine((engine) => engine.commit_tx(toWasmU64(txId)), { allowDuringMaintenance: true });
            this.committedIndexes = cloneNormalizedIndexDefinitions(normalized);
        } catch (error) {
            try {
                this.withEngine((engine) => engine.rollback_tx(toWasmU64(txId)), { allowDuringMaintenance: true });
            } catch {}
            throw error;
        } finally {
            this.cleanupTxState(txId);
        }
    }
    /**
     * Compaction publishes a new generation with a strict swap:
     * 1. read the active generation (a corrupt control file aborts here);
     * 2. stream every live row into a fresh generation inside the engine;
     * 3. compare-and-swap the control file against the generation read in 1;
     * 4. if the swap reports an error, re-read the control file to learn
     *    which generation is actually active before deciding what to clean up.
     * The old engine is abandoned, not closed: its files are no longer
     * active, so it must not checkpoint into them.
     */
    private async runCompactionOperation(operation: MaintenanceOperation): Promise<CompactionResult> {
        const dbName = this.requireDbName();
        if (this.maintenanceOperation) {
            throw remoteError(
                'DatabaseBusyError',
                `database ${dbName} is busy: ${this.maintenanceOperation} in progress`
            );
        }
        this.maintenanceOperation = operation;
        const start = performance.now();
        const wasm = await this.loadWasm();
        let rebuiltEngine: WasmEngine | null = null;
        let generationName: string | null = null;
        let published = false;
        try {
            const sizeBefore = await wasm.dbDirectorySize(dbName);
            const defs = cloneNormalizedIndexDefinitions(this.loadCommittedIndexSchema());
            this.rollbackAllTransactions({ allowDuringMaintenance: true });
            const expectedGeneration = await wasm.readActiveGeneration(dbName);
            generationName = (await wasm.prepareRebuildTarget(dbName)).generationName;
            rebuiltEngine = new wasm.WasmEngine();
            await rebuiltEngine.openGeneration(dbName, generationName, {
                create_if_missing: true,
                cache_pages: this.currentCachePages()
            });
            const target = rebuiltEngine;
            this.withEngine((engine) => engine.compact_into(target), { allowDuringMaintenance: true });
            if (operation === 'rebuild' && defs.length > 0) {
                // compact copies index entries as they are; rebuild regenerates
                // them from the rows so stale entries do not survive.
                const previousEngine = this.engine;
                const previousMaintenance = this.maintenanceOperation;
                this.engine = target;
                this.maintenanceOperation = null;
                try {
                    this.committedIndexes = [];
                    this.committedStoreCompression = null;
                    await this.applyCommittedIndexSchema(defs);
                } finally {
                    this.engine = previousEngine;
                    this.maintenanceOperation = previousMaintenance;
                    this.committedStoreCompression = null;
                }
            }
            try {
                await wasm.swapActiveGeneration(dbName, generationName, expectedGeneration);
                published = true;
            } catch (swapError) {
                published = (await readActiveGenerationOrNull(wasm, dbName)) === generationName;
                if (!published) {
                    throw swapError;
                }
            }
            const previousEngine = this.engine;
            this.engine = rebuiltEngine;
            rebuiltEngine = null;
            this.committedIndexes = defs;
            this.committedStoreCompression = null;
            try {
                previousEngine?.abandon();
            } catch {}
            try {
                await wasm.cleanupInactiveEntries(dbName);
            } catch {}
            const sizeAfter = await wasm.dbDirectorySize(dbName);
            return {
                sizeBefore,
                sizeAfter,
                reclaimedBytes: Math.max(0, sizeBefore - sizeAfter),
                durationMs: performance.now() - start
            };
        } catch (error) {
            if (!published) {
                this.committedIndexes = null;
                this.committedStoreCompression = null;
                try {
                    rebuiltEngine?.abandon();
                } catch {}
                if (generationName !== null) {
                    // Only unpublished generations are removed; if the control
                    // file cannot be read, nothing is deleted.
                    const active = await readActiveGenerationOrNull(wasm, dbName);
                    if (active !== undefined && active !== generationName) {
                        try {
                            await wasm.cleanupInactiveEntries(dbName);
                        } catch {}
                    }
                }
            }
            throw remapError(error);
        } finally {
            this.maintenanceOperation = null;
        }
    }
    /**
     * Brings a poisoned engine back to a trusted state. Returns `null` when
     * the engine was healthy. All open transactions are gone afterwards.
     */
    private recoverEngineIfNeeded(): { pendingTxid: number | null; pendingCommitted: boolean } | null {
        const engine = this.engine;
        if (!engine || !engine.needs_recovery()) {
            return null;
        }
        let report: WasmRecoveryReport;
        try {
            report = engine.recover();
        } catch (error) {
            throw remoteError(
                'RecoveryRequiredError',
                `database ${this.dbName ?? ''} needs recovery and recovery failed: ${remapError(error).message}`
            );
        }
        this.clearRuntimeCaches();
        return {
            pendingTxid: fromOptionalWasmU64(report.pendingTxid),
            pendingCommitted: report.pendingCommitted === true
        };
    }
    private cleanupTxState(txId: number): void {
        this.txChanges.delete(txId);
        this.txModes.delete(txId);
        this.txIndexSchemas.delete(txId);
        this.txIndexSchemaChanged.delete(txId);
        this.txStoreCompression.delete(txId);
    }
    private getOrCreateTrackedTxn(txId: number): TrackedTxnChanges {
        return getOrInsert(this.txChanges, txId, () => new Map<string, TrackedStoreChanges>());
    }
    private getOrCreateTrackedStore(txId: number, store: string): TrackedStoreChanges {
        return getOrInsert(this.getOrCreateTrackedTxn(txId), store, () => ({
            touched: false,
            storeLevel: null,
            keys: new Map()
        }));
    }
    private recordStoreLevelChange(txId: number, store: string, kind: 'clear' | 'drop'): void {
        if (isInternalStoreName(store)) {
            return;
        }
        const trackedStore = this.getOrCreateTrackedStore(txId, store);
        trackedStore.touched = true;
        trackedStore.storeLevel = kind;
        // Earlier key changes are subsumed by the store-level change.
        trackedStore.keys.clear();
    }
    private recordPutChange(txId: number, store: string, key: Uint8Array, baselineExists: boolean): void {
        this.recordKeyChange(txId, store, key, baselineExists, 'put');
    }
    private recordPutChanges(
        txId: number,
        store: string,
        entries: Array<[Uint8Array, Uint8Array]>,
        baselines: boolean[]
    ): void {
        if (isInternalStoreName(store)) {
            return;
        }
        const trackedStore = this.getOrCreateTrackedStore(txId, store);
        trackedStore.touched = true;
        const count = Math.min(entries.length, baselines.length);
        for (let i = 0; i < count; i += 1) {
            this.setTrackedKeyChange(trackedStore, entries[i][0], baselines[i], 'put');
        }
    }
    private recordPutKeyChanges(
        txId: number,
        store: string,
        keys: Uint8Array[],
        baselines: ArrayLike<boolean | number>
    ): void {
        if (isInternalStoreName(store)) {
            return;
        }
        const trackedStore = this.getOrCreateTrackedStore(txId, store);
        trackedStore.touched = true;
        const count = Math.min(keys.length, baselines.length);
        for (let i = 0; i < count; i += 1) {
            this.setTrackedKeyChange(trackedStore, keys[i], baselines[i] === true || baselines[i] === 1, 'put');
        }
    }
    private recordDeleteChange(txId: number, store: string, key: Uint8Array, deleted: boolean): void {
        if (deleted) {
            this.recordKeyChange(txId, store, key, true, 'delete');
        }
    }
    private recordKeyChange(
        txId: number,
        store: string,
        key: Uint8Array,
        baselineExists: boolean,
        finalKind: DbChange['kind']
    ): void {
        if (isInternalStoreName(store)) {
            return;
        }
        const trackedStore = this.getOrCreateTrackedStore(txId, store);
        trackedStore.touched = true;
        this.setTrackedKeyChange(trackedStore, key, baselineExists, finalKind);
    }
    private setTrackedKeyChange(
        trackedStore: TrackedStoreChanges,
        key: Uint8Array,
        baselineExists: boolean,
        finalKind: DbChange['kind']
    ): void {
        const keyId = bytesToHex(key);
        const existing = trackedStore.keys.get(keyId);
        trackedStore.keys.set(keyId, {
            key: existing?.key ?? key.slice(),
            baselineExists: existing?.baselineExists ?? baselineExists,
            finalKind
        });
    }
    private markStoreTouched(txId: number, store: string): void {
        if (isInternalStoreName(store)) {
            return;
        }
        this.getOrCreateTrackedStore(txId, store).touched = true;
    }
    private recordBatchOutcomes(
        txId: number,
        store: string,
        ops: Array<BatchOp>,
        outcomes: Array<WasmBatchOutcome>
    ): void {
        for (let i = 0; i < outcomes.length && i < ops.length; i += 1) {
            const op = ops[i];
            const outcome = outcomes[i];
            if (op.kind === 'put' && outcome.kind === 'put') {
                this.recordPutChange(txId, store, op.key, outcome.baselineExists);
            } else if (op.kind === 'delete' && outcome.kind === 'delete') {
                this.recordDeleteChange(txId, store, op.key, outcome.deleted);
            }
        }
    }
    private recordBatchKeyOutcomes(
        txId: number,
        store: string,
        ops: PackedBatchOpKey[],
        outcomes: Array<WasmBatchOutcome>
    ): void {
        for (let i = 0; i < outcomes.length && i < ops.length; i += 1) {
            const op = ops[i];
            const outcome = outcomes[i];
            if (op.kind === 'put' && outcome.kind === 'put') {
                this.recordPutChange(txId, store, op.key, outcome.baselineExists);
            } else if (op.kind === 'delete' && outcome.kind === 'delete') {
                this.recordDeleteChange(txId, store, op.key, outcome.deleted);
            }
        }
    }
    private finalizeTrackedChanges(tracked: TrackedTxnChanges): StoreChangeSet[] {
        const stores: StoreChangeSet[] = [];
        for (const [store, trackedStore] of tracked) {
            if (!trackedStore.touched || isInternalStoreName(store)) {
                continue;
            }
            const changes: DbChange[] = [];
            if (trackedStore.storeLevel !== null) {
                changes.push({ key: new Uint8Array(0), kind: trackedStore.storeLevel });
            }
            for (const change of trackedStore.keys.values()) {
                if (change.finalKind === 'delete' && !change.baselineExists) {
                    continue;
                }
                changes.push({
                    key: change.key,
                    kind: change.finalKind
                });
            }
            stores.push({ store, changes });
        }
        return stores;
    }
    private rollbackAllTransactions(
        options: {
            allowDuringMaintenance?: boolean;
        } = {}
    ): void {
        if (!this.engine || this.txChanges.size === 0) {
            this.clearTxCaches();
            return;
        }
        const txIds = Array.from(this.txChanges.keys());
        for (const txId of txIds) {
            try {
                this.withEngine((engine) => engine.rollback_tx(toWasmU64(txId)), options);
            } finally {
                this.cleanupTxState(txId);
            }
        }
    }
    private currentStoreNames(): string[] {
        return this.allRawStoreNames().filter((store) => !isInternalStoreName(store));
    }
    private allRawStoreNames(): string[] {
        return this.withEngine((engine) => engine.list_stores()) as string[];
    }
    private loadCommittedStoreCompression(): Map<string, CompressionOption> {
        if (this.committedStoreCompression) {
            return this.committedStoreCompression;
        }
        const rows = this.withEngine((engine) => engine.list_store_configs()) as Array<{
            name: string;
            flags: number | bigint;
        }>;
        const loaded = new Map<string, CompressionOption>();
        for (const row of rows) {
            if (typeof row.name !== 'string') {
                throw remoteError('CorruptionError', 'invalid store config row: missing name');
            }
            const flags = typeof row.flags === 'bigint' ? Number(row.flags) : row.flags;
            if (typeof flags !== 'number' || !Number.isSafeInteger(flags) || flags < 0) {
                throw remoteError('CorruptionError', `invalid store flags for ${row.name}`);
            }
            loaded.set(row.name, compressionFromStoreFlags(flags));
        }
        this.committedStoreCompression = loaded;
        return loaded;
    }
    private ensureTxStoreCompression(txId: number): Map<string, CompressionOption> {
        return getOrInsert(this.txStoreCompression, txId, () => new Map<string, CompressionOption>());
    }
    private recordStoreCompressionCreate(txId: number, store: string, compression: CompressionOption): void {
        if (isInternalStoreName(store)) {
            return;
        }
        this.ensureTxStoreCompression(txId).set(store, compression);
    }
    private recordStoreCompressionDrop(txId: number, store: string): void {
        if (isInternalStoreName(store)) {
            return;
        }
        this.ensureTxStoreCompression(txId).delete(store);
    }
    private storeCompressionForTx(txId: number, store: string): CompressionOption {
        if (isInternalStoreName(store)) {
            return false;
        }
        const snapshot = this.txStoreCompression.get(txId);
        if (snapshot) {
            return snapshot.get(store) ?? false;
        }
        return this.loadCommittedStoreCompression().get(store) ?? false;
    }
    private async encodeEntriesForWrite(
        txId: number,
        store: string,
        entries: Array<[Uint8Array, Uint8Array]>
    ): Promise<Array<[Uint8Array, Uint8Array]>> {
        if (isInternalStoreName(store)) {
            return entries;
        }
        const compression = this.storeCompressionForTx(txId, store);
        if (compression === false) {
            return entries;
        }
        const storedEntries: Array<[Uint8Array, Uint8Array]> = [];
        for (const [key, value] of entries) {
            storedEntries.push([key, await encodeStoreValueRecord(value, compression)]);
        }
        return storedEntries;
    }
    private async encodeBatchOpsForWrite(txId: number, store: string, ops: Array<BatchOp>): Promise<Array<BatchOp>> {
        if (isInternalStoreName(store)) {
            return ops;
        }
        const compression = this.storeCompressionForTx(txId, store);
        if (compression === false) {
            return ops;
        }
        const storedOps: Array<BatchOp> = [];
        for (const op of ops) {
            storedOps.push(
                op.kind === 'put'
                    ? { kind: 'put', key: op.key, value: await encodeStoreValueRecord(op.value, compression) }
                    : op
            );
        }
        return storedOps;
    }
    private async encodeStoreValueForWrite(txId: number, store: string, value: Uint8Array): Promise<Uint8Array> {
        if (isInternalStoreName(store)) {
            return value;
        }
        return encodeStoreValueRecord(value, this.storeCompressionForTx(txId, store));
    }
    private async decodeStoreValue(txId: number, store: string, value: Uint8Array): Promise<Uint8Array> {
        if (isInternalStoreName(store)) {
            return value;
        }
        const compression = this.storeCompressionForTx(txId, store);
        if (compression === false) {
            return value;
        }
        return decodeStoreValueRecord(value, { strict: true });
    }
    private async decodeStoreValueForFeed(store: string, value: Uint8Array): Promise<Uint8Array> {
        if (isInternalStoreName(store)) {
            return value;
        }
        const compression = this.loadCommittedStoreCompression().get(store) ?? false;
        return decodeStoreValueRecord(value, { strict: compression !== false });
    }
    private async readStoreValue(txId: number, store: string, key: Uint8Array): Promise<Uint8Array | null> {
        const value = this.readRawValue(txId, store, key);
        return value === null ? null : this.decodeStoreValue(txId, store, value);
    }
    private async readStoreScan(txId: number, store: string, range: Range): Promise<ScanItem[]> {
        const rows = this.readRawScan(txId, store, range);
        if (isInternalStoreName(store) || this.storeCompressionForTx(txId, store) === false) {
            return rows;
        }
        return await Promise.all(
            rows.map(async (row) => ({
                key: row.key,
                value: await this.decodeStoreValue(txId, store, row.value)
            }))
        );
    }
    private indexesForStoreInTx(txId: number, store: string): NormalizedIndexDef[] {
        return indexesForStore(this.indexSchemaForTx(txId), store);
    }
    private resolveIndexDefinition(txId: number, store: string, indexName: string): NormalizedIndexDef {
        if (!this.rawStoreExistsInTx(txId, store)) {
            throw remoteError('StoreNotFoundError', `store not found: ${store}`);
        }
        const def = findIndexDefinition(this.indexSchemaForTx(txId), store, indexName);
        if (!def) {
            throw remoteError('IndexNotFoundError', `index not found: ${store}.${indexName}`);
        }
        return def;
    }
    private indexSchemaForTx(txId: number): NormalizedIndexDef[] {
        const override = this.txIndexSchemas.get(txId);
        if (override) {
            return override;
        }
        if (this.committedIndexes) {
            return this.committedIndexes;
        }
        const loaded = this.readIndexSchemaInTx(txId);
        this.committedIndexes = cloneNormalizedIndexDefinitions(loaded);
        return this.committedIndexes;
    }
    private loadCommittedIndexSchema(): NormalizedIndexDef[] {
        if (this.committedIndexes) {
            return this.committedIndexes;
        }
        const txId = fromWasmU64(this.withEngine((engine) => engine.begin_tx('readonly')));
        try {
            const loaded = this.readIndexSchemaInTx(txId);
            this.committedIndexes = cloneNormalizedIndexDefinitions(loaded);
            return this.committedIndexes;
        } finally {
            this.withEngine((engine) => engine.rollback_tx(toWasmU64(txId)));
        }
    }
    private readIndexSchemaInTx(txId: number): NormalizedIndexDef[] {
        if (!this.rawStoreExistsInTx(txId, INDEX_METADATA_STORE)) {
            return [];
        }
        const rows = this.readRawScan(txId, INDEX_METADATA_STORE, EMPTY_RANGE);
        const defs = rows.map((row) => decodeIndexMetadataValue(row.value));
        defs.sort(compareNormalizedIndexDefinitions);
        return defs;
    }
    private rawStoreExistsInTx(txId: number, store: string): boolean {
        try {
            this.readRawScan(txId, store, { limit: 0 });
            return true;
        } catch (error) {
            if (isNamedError(error, 'StoreNotFoundError')) {
                return false;
            }
            throw error;
        }
    }
    private ensureRawStore(txId: number, store: string): void {
        this.ignoreNamedError('StoreExistsError', () => {
            this.withEngine((engine) => engine.create_store(toWasmU64(txId), store));
        });
    }
    private clearRawStoreIfExists(txId: number, store: string): void {
        this.ignoreNamedError('StoreNotFoundError', () => {
            this.withEngine((engine) => engine.clear_store(toWasmU64(txId), store));
        });
    }
    private dropRawStoreIfExists(txId: number, store: string): void {
        this.ignoreNamedError('StoreNotFoundError', () => {
            this.withEngine((engine) => engine.drop_store(toWasmU64(txId), store));
        });
    }
    private ensureInternalStoresForDefinitions(txId: number, defs: ReadonlyArray<NormalizedIndexDef>): void {
        for (const def of defs) {
            this.ensureRawStore(txId, def.internalStore);
        }
    }
    private ensureInternalStoresForStore(txId: number, store: string): void {
        this.ensureInternalStoresForDefinitions(txId, this.indexesForStoreInTx(txId, store));
    }
    private readRawValue(txId: number, store: string, key: Uint8Array): Uint8Array | null {
        const value = this.withEngine((engine) => engine.get(toWasmU64(txId), store, key));
        return value === null ? null : normalizeWasmBytes(value);
    }
    private readRawScan(txId: number, store: string, range: Range): ScanItem[] {
        const rows = this.withEngine((engine) => engine.scan(toWasmU64(txId), store, toWasmRange(range))) as ScanItem[];
        return rows.map((row) => ({
            key: normalizeWasmBytes(row.key),
            value: normalizeWasmBytes(row.value)
        }));
    }
    private putRawValue(txId: number, store: string, key: Uint8Array, value: Uint8Array): void {
        this.withEngine((engine) => engine.put(toWasmU64(txId), store, key, value));
    }
    private putIndexEntry(txId: number, def: NormalizedIndexDef, physicalKey: Uint8Array, value: Uint8Array): void {
        this.putRawValue(txId, def.internalStore, physicalKey, value);
    }
    private deleteIndexEntryIfPresent(
        txId: number,
        def: NormalizedIndexDef,
        logicalKey: Uint8Array,
        primaryKey: Uint8Array
    ): void {
        const physicalKey = this.buildIndexEntryKey(def, logicalKey, primaryKey);
        this.ignoreNamedError('StoreNotFoundError', () => {
            this.withEngine((engine) => engine.delete(toWasmU64(txId), def.internalStore, physicalKey));
        });
    }
    private buildIndexEntryKey(def: NormalizedIndexDef, logicalKey: Uint8Array, primaryKey: Uint8Array): Uint8Array {
        const physicalKey = encodeIndexEntryKey(logicalKey, primaryKey);
        if (physicalKey.byteLength > MAX_ENGINE_KEY_BYTES) {
            throw remoteError('KeyTooLargeError', `index key too large for ${def.store}.${def.name}`);
        }
        return physicalKey;
    }
    private async assertUniqueIndexAvailability(
        txId: number,
        def: NormalizedIndexDef,
        logicalKey: Uint8Array,
        primaryKey: Uint8Array
    ): Promise<void> {
        if (!def.unique) {
            return;
        }
        const rows = this.readRawScan(txId, def.internalStore, indexKeyExactRange(logicalKey));
        for (const row of rows) {
            const decoded = decodeIndexEntryKey(row.key);
            if (bytesEqual(decoded.primaryKey, primaryKey)) {
                continue;
            }
            const value = await this.readStoreValue(txId, def.store, decoded.primaryKey);
            if (value === null) {
                this.cleanupStaleIndexRow(txId, def, row.key);
                continue;
            }
            const currentLogicalKey = extractLogicalIndexKey(def, value);
            if (currentLogicalKey === null || !bytesEqual(currentLogicalKey, logicalKey)) {
                this.cleanupStaleIndexRow(txId, def, row.key);
                continue;
            }
            throw remoteError(
                'UniqueIndexConstraintError',
                `unique index constraint violation on ${def.store}.${def.name}`
            );
        }
    }
    /**
     * Reads up to `limit` live rows of an index range, starting after
     * `cursor` (a physical index key from a previous page). Raw index entries
     * are fetched in bounded chunks, so stale entries skipped along the way
     * never force the whole range into memory.
     */
    private async loadVisibleIndexPage(
        txId: number,
        def: NormalizedIndexDef,
        range: Range,
        cursor: Uint8Array | null,
        limit: number
    ): Promise<IndexScanPage> {
        const { limit: _ignored, ...withoutLimit } = range;
        const physical = indexRangeToPhysicalRange(withoutLimit);
        const reverse = physical.reverse === true;
        let resumeAfter = cursor;
        const visible: ScanItem[] = [];
        for (;;) {
            const chunkRange: Range = { ...physical, limit: INDEX_SCAN_RAW_CHUNK_ROWS };
            if (resumeAfter !== null) {
                if (reverse) {
                    delete chunkRange.lte;
                    chunkRange.lt = resumeAfter;
                } else {
                    delete chunkRange.gte;
                    chunkRange.gt = resumeAfter;
                }
            }
            let rawRows: ScanItem[];
            try {
                rawRows = this.readRawScan(txId, def.internalStore, chunkRange);
            } catch (error) {
                if (isNamedError(error, 'StoreNotFoundError')) {
                    return { rows: [], cursor: null };
                }
                throw error;
            }
            for (const row of rawRows) {
                resumeAfter = row.key;
                const resolved = await this.resolveVisibleIndexedRow(txId, def, row.key);
                if (!resolved) {
                    continue;
                }
                visible.push(resolved);
                if (visible.length >= limit) {
                    return { rows: visible, cursor: row.key.slice() };
                }
            }
            if (rawRows.length < INDEX_SCAN_RAW_CHUNK_ROWS) {
                return { rows: visible, cursor: null };
            }
        }
    }
    private async resolveVisibleIndexedRow(
        txId: number,
        def: NormalizedIndexDef,
        physicalKey: Uint8Array
    ): Promise<ScanItem | null> {
        const decoded = decodeIndexEntryKey(physicalKey);
        const value = await this.readStoreValue(txId, def.store, decoded.primaryKey);
        if (value === null) {
            this.cleanupStaleIndexRow(txId, def, physicalKey);
            return null;
        }
        const currentLogicalKey = extractLogicalIndexKey(def, value);
        if (currentLogicalKey === null || !bytesEqual(currentLogicalKey, decoded.logicalKey)) {
            this.cleanupStaleIndexRow(txId, def, physicalKey);
            return null;
        }
        return {
            key: decoded.primaryKey.slice(),
            value
        };
    }
    private cleanupStaleIndexRow(txId: number, def: NormalizedIndexDef, physicalKey: Uint8Array): void {
        if (this.txModes.get(txId) !== 'readwrite') {
            return;
        }
        try {
            this.withEngine((engine) => engine.delete(toWasmU64(txId), def.internalStore, physicalKey));
        } catch {}
    }
    private async loadWasm(): Promise<WasmModule> {
        if (!this.wasmReady) {
            this.wasmReady = (async () => {
                // Absolute URLs stay outside Vite's ?import transform, which rejects JS in /public.
                const moduleUrl = new URL('/engine/moyodb_engine.js', import.meta.url).href;
                const wasmUrl = new URL('/engine/moyodb_engine_bg.wasm', import.meta.url).href;
                const wasmModule = (await import(
                    /* @vite-ignore */
                    moduleUrl
                )) as WasmModule;
                await wasmModule.default({ module_or_path: wasmUrl });
                return wasmModule;
            })();
        }
        return this.wasmReady;
    }
    private withEngine<T>(
        fn: (engine: NonNullable<DbWorker['engine']>) => T,
        options: {
            allowDuringMaintenance?: boolean;
        } = {}
    ): T {
        if (this.maintenanceOperation && !options.allowDuringMaintenance) {
            this.ensureNotInMaintenance();
        }
        if (!this.engine) {
            throw remoteError('InternalError', 'engine is not open');
        }
        try {
            return fn(this.engine);
        } catch (err) {
            throw remapError(err);
        }
    }
    private ignoreNamedError(name: string, fn: () => void): void {
        try {
            fn();
        } catch (error) {
            if (isNamedError(error, name)) {
                return;
            }
            throw error;
        }
    }
    private clearTxCaches(): void {
        this.txChanges.clear();
        this.txModes.clear();
        this.txIndexSchemas.clear();
        this.txIndexSchemaChanged.clear();
        this.txStoreCompression.clear();
    }
    private clearRuntimeCaches(): void {
        this.clearTxCaches();
        this.committedIndexes = null;
        this.committedStoreCompression = null;
    }
    private async cleanupAfterFailedOpen(engine: WasmEngine | null): Promise<void> {
        try {
            engine?.abandon();
        } catch {}
        this.engine = null;
        this.clearRuntimeCaches();
        this.events?.close();
        this.events = null;
        if (this.lease) {
            try {
                await this.lease.release();
            } catch {}
        }
        this.lease = null;
        this.disposePersistenceBridge();
        this.dbName = null;
        this.openOptions = null;
        this.maintenanceOperation = null;
    }
    private disposePersistenceBridge(): void {
        this.persistenceBridge.close();
    }
}
async function assertCapabilities(): Promise<void> {
    if (typeof navigator.storage.getDirectory !== 'function') {
        throw remoteError('UnsupportedPlatformError', 'navigator.storage.getDirectory is unavailable');
    }
    if (typeof BroadcastChannel === 'undefined') {
        throw remoteError('UnsupportedPlatformError', 'BroadcastChannel is unavailable');
    }
    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle('__moyodb_capability__', { create: true });
    const file = await dir.getFileHandle('probe.bin', { create: true });
    if (typeof file.createSyncAccessHandle !== 'function') {
        throw remoteError('UnsupportedPlatformError', 'createSyncAccessHandle is unavailable');
    }
    const handle = await file.createSyncAccessHandle();
    handle.close();
    try {
        await dir.removeEntry('probe.bin');
    } catch {}
}
function assertSecureContext() {
    if (!self.isSecureContext) {
        throw remoteError('UnsupportedPlatformError', 'moyodb requires a secure context (HTTPS)');
    }
}
function remoteError(code: string, message: string): Error {
    const error = new Error(message);
    error.name = code;
    Object.defineProperty(error, 'code', {
        value: code,
        enumerable: true,
        configurable: true
    });
    return error;
}
function applyChangeFeedPolicy(engine: WasmEngine, settings: Required<ChangeFeedSettings>): void {
    const current = engine.change_feed_policy();
    const currentRetain = fromOptionalWasmU64(current.retainTxids);
    if (current.enabled === settings.enabled && (!settings.enabled || currentRetain === settings.retainTxids)) {
        return;
    }
    const txId = engine.begin_tx('readwrite');
    try {
        engine.set_change_feed_policy(txId, { enabled: settings.enabled, retainTxids: settings.retainTxids });
        engine.commit_tx(txId);
    } catch (error) {
        try {
            engine.rollback_tx(txId);
        } catch {}
        throw remapError(error);
    }
}
/** Active generation name, `null` without a control file, `undefined` when it cannot be read. */
async function readActiveGenerationOrNull(wasm: WasmModule, dbName: string): Promise<string | null | undefined> {
    try {
        return await wasm.readActiveGeneration(dbName);
    } catch {
        return undefined;
    }
}
function normalizeEngineHealth(health: WasmEngineHealth): EngineHealth {
    if (health.state === 'recoveryRequired') {
        return {
            state: 'recoveryRequired',
            reason: health.reason,
            pendingTxid: fromOptionalWasmU64(health.pendingTxid)
        };
    }
    return { state: health.state };
}
function partialBooleanOutcomes(error: unknown): boolean[] {
    if (!isRecord(error) || !Array.isArray(error.partial)) {
        return [];
    }
    return error.partial.filter((value): value is boolean => typeof value === 'boolean');
}
function partialBatchOutcomes(error: unknown): WasmBatchOutcome[] {
    if (!isRecord(error) || !Array.isArray(error.partial)) {
        return [];
    }
    return error.partial.filter((value): value is WasmBatchOutcome => {
        if (!isRecord(value) || typeof value.kind !== 'string') {
            return false;
        }
        return (
            (value.kind === 'put' && typeof value.baselineExists === 'boolean') ||
            (value.kind === 'delete' && typeof value.deleted === 'boolean')
        );
    });
}
function remapError(err: unknown): Error {
    if (err instanceof Error) {
        return err;
    }
    if (isRecord(err)) {
        const code = typeof err.code === 'string' ? err.code : typeof err.name === 'string' ? err.name : 'Error';
        const message = typeof err.message === 'string' ? err.message : 'Unknown error';
        const error = remoteError(code, message);
        if ('partial' in err) {
            Object.defineProperty(error, 'partial', {
                value: err.partial,
                enumerable: true,
                configurable: true
            });
        }
        return error;
    }
    return remoteError('Error', String(err));
}
exposeWorkerApi(new DbWorker());
