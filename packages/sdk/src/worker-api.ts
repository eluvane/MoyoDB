import type {
    BatchOp,
    ChangeFeed,
    ChangeFeedOptions,
    CompactionResult,
    CreateStoreOptions,
    DebugFailpoint,
    DbStats,
    ExportSnapshotOptions,
    IndexDef,
    PutOptions,
    Range,
    ScanItem,
    StorageInfo,
    TxMode
} from './types';
export interface WorkerRuntimeOpenOptions {
    createIfMissing: boolean;
    ownerWaitMs: number;
    requestPersistence: boolean;
    cachePages: number;
    debugFailpoint: DebugFailpoint;
}
export interface WorkerOpenRequest {
    dbName: string;
    options: WorkerRuntimeOpenOptions;
}
export interface WorkerApi {
    open(request: WorkerOpenRequest): Promise<void>;
    close(): Promise<void>;
    destroy(): Promise<void>;
    deleteDB(dbName: string): Promise<void>;
    begin(mode: TxMode): Promise<number>;
    commit(txId: number): Promise<number>;
    rollback(txId: number): Promise<void>;
    createStore(txId: number, name: string, options?: CreateStoreOptions): Promise<void>;
    dropStore(txId: number, name: string): Promise<void>;
    clearStore(txId: number, name: string): Promise<void>;
    get(txId: number, store: string, key: Uint8Array): Promise<Uint8Array | null>;
    getMany(txId: number, store: string, keys: Array<Uint8Array>): Promise<Array<Uint8Array | null>>;
    has(txId: number, store: string, key: Uint8Array): Promise<boolean>;
    put(txId: number, store: string, key: Uint8Array, value: Uint8Array, options?: PutOptions): Promise<void>;
    putMany(txId: number, store: string, entries: Array<[Uint8Array, Uint8Array]>, options?: PutOptions): Promise<void>;
    delete(txId: number, store: string, key: Uint8Array): Promise<boolean>;
    deleteMany(txId: number, store: string, keys: Array<Uint8Array>): Promise<void>;
    applyBatch(txId: number, store: string, ops: Array<BatchOp>): Promise<void>;
    scan(txId: number, store: string, range: Range): Promise<ScanItem[]>;
    getByIndex(txId: number, store: string, indexName: string, key: Uint8Array): Promise<Uint8Array | null>;
    scanByIndex(txId: number, store: string, indexName: string, range: Range): Promise<ScanItem[]>;
    getIndexes(): Promise<IndexDef[]>;
    reconcileIndexes(txId: number, indexes: IndexDef[]): Promise<void>;
    listStores(): Promise<string[]>;
    getVersion(): Promise<number>;
    changesSince(txId: number, options: ChangeFeedOptions): Promise<ChangeFeed>;
    setSchemaVersion(txId: number, version: number): Promise<void>;
    exportSnapshot(options?: ExportSnapshotOptions): Promise<Uint8Array>;
    importSnapshot(data: Uint8Array): Promise<void>;
    reset(): Promise<void>;
    compact(): Promise<CompactionResult>;
    rebuild(): Promise<CompactionResult>;
    stats(): Promise<DbStats>;
    storageInfo(): Promise<StorageInfo>;
    requestPersistence(): Promise<boolean>;
    setFailpoint(failpoint: DebugFailpoint): Promise<void>;
}
