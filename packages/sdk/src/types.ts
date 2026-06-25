export type TxMode = 'readonly' | 'readwrite';
export type TxId = number;
export type DebugFailpoint = 'after_wal_flush' | 'after_main_flush' | 'before_superblock_flush' | null;
export type ChangeKind = 'put' | 'delete';
export type Unsubscribe = () => void;
export type BatchOp =
    | {
          kind: 'put';
          key: Uint8Array;
          value: Uint8Array;
      }
    | {
          kind: 'delete';
          key: Uint8Array;
      };
export type MigrateHook = (context: MigrationContext) => Promise<void> | void;
export interface DbChange {
    key: Uint8Array;
    kind: ChangeKind;
}
export type DbSubscriptionCallback = (store: string, changes: DbChange[], txnId: number) => void;
export interface Range {
    gt?: Uint8Array;
    gte?: Uint8Array;
    lt?: Uint8Array;
    lte?: Uint8Array;
    reverse?: boolean;
    limit?: number;
}
export interface IndexDef {
    store: string;
    name: string;
    keyPath: string | string[];
    unique?: boolean;
}
export interface OpenOptions {
    createIfMissing?: boolean;
    ownerWaitMs?: number;
    requestPersistence?: boolean;
    cachePages?: number;
    debugFailpoint?: DebugFailpoint;
    version?: number;
    migrate?: MigrateHook;
    indexes?: IndexDef[];
}
export interface PutOptions {
    ttl?: number;
}
export type CompressionKind = 'gzip' | 'deflate';
export interface CreateStoreOptions {
    compression?: CompressionKind | false;
}
export interface ExportSnapshotOptions {
    compression?: CompressionKind | false;
}
export interface ChangeRecord {
    txId: TxId;
    store: string;
    key: Uint8Array;
    kind: ChangeKind;
    value?: Uint8Array;
}
export interface ChangeFeedOptions {
    stores?: string[];
    limit?: number;
}
export interface ChangeFeed {
    changes: ChangeRecord[];
    latestTxId: TxId;
}
export interface DbStats {
    db_name: string;
    db_id: number;
    page_size: number;
    catalog_root_page_id: number;
    next_page_id: number;
    last_committed_txid: number;
    last_replayed_wal_offset: number;
    store_count: number;
    manifest_len: number;
    main_len: number;
    wal_len: number;
    active_txns: number;
    write_tx_open: boolean;
    cache_pages: number;
}
export interface StorageInfo {
    dbSize: number;
    originUsage: number;
    originQuota: number;
    persisted: boolean;
}
export interface CompactionResult {
    sizeBefore: number;
    sizeAfter: number;
    reclaimedBytes: number;
    durationMs: number;
}
export interface ScanItem {
    key: Uint8Array;
    value: Uint8Array;
}
export interface Transaction {
    readonly mode: TxMode;
    get(store: string, key: Uint8Array): Promise<Uint8Array | null>;
    getMany(store: string, keys: Array<Uint8Array>): Promise<Array<Uint8Array | null>>;
    has(store: string, key: Uint8Array): Promise<boolean>;
    put(store: string, key: Uint8Array, value: Uint8Array, options?: PutOptions): Promise<void>;
    putMany(store: string, entries: Array<[key: Uint8Array, value: Uint8Array]>, options?: PutOptions): Promise<void>;
    delete(store: string, key: Uint8Array): Promise<boolean>;
    deleteMany(store: string, keys: Array<Uint8Array>): Promise<void>;
    applyBatch(store: string, ops: Array<BatchOp>): Promise<void>;
    scan(store: string, range?: Range): Promise<ScanItem[]>;
    getByIndex(store: string, indexName: string, key: Uint8Array): Promise<Uint8Array | null>;
    scanByIndex(store: string, indexName: string, range?: Range): AsyncIterable<[key: Uint8Array, value: Uint8Array]>;
    createStore(name: string, options?: CreateStoreOptions): Promise<void>;
    dropStore(name: string): Promise<void>;
    clearStore(name: string): Promise<void>;
    commit(): Promise<void>;
    rollback(): Promise<void>;
}
export interface MigrationContext {
    db: DB;
    oldVersion: number;
    newVersion: number;
    transaction: Transaction;
}
export interface DB {
    begin(mode?: TxMode): Promise<Transaction>;
    createStore(name: string, options?: CreateStoreOptions): Promise<void>;
    dropStore(name: string): Promise<void>;
    clearStore(name: string): Promise<void>;
    listStores(): Promise<string[]>;
    getVersion(): Promise<number>;
    changesSince(txid: TxId, options?: ChangeFeedOptions): Promise<ChangeFeed>;
    get(store: string, key: Uint8Array): Promise<Uint8Array | null>;
    getMany(store: string, keys: Array<Uint8Array>): Promise<Array<Uint8Array | null>>;
    has(store: string, key: Uint8Array): Promise<boolean>;
    put(store: string, key: Uint8Array, value: Uint8Array, options?: PutOptions): Promise<void>;
    delete(store: string, key: Uint8Array): Promise<boolean>;
    scan(store: string, range?: Range): Promise<ScanItem[]>;
    exportSnapshot(options?: ExportSnapshotOptions): Promise<Uint8Array>;
    importSnapshot(data: Uint8Array): Promise<void>;
    reset(): Promise<void>;
    compact(): Promise<CompactionResult>;
    rebuild(): Promise<CompactionResult>;
    stats(): Promise<DbStats>;
    storageInfo(): Promise<StorageInfo>;
    requestPersistence(): Promise<boolean>;
    close(): Promise<void>;
    destroy(): Promise<void>;
    setFailpoint(failpoint: DebugFailpoint): Promise<void>;
    subscribe(callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(storeName: string, callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(storeName: string, keyPrefix: Uint8Array, callback: DbSubscriptionCallback): Unsubscribe;
    watch(callback: DbSubscriptionCallback): Unsubscribe;
    watch(storeName: string, callback: DbSubscriptionCallback): Unsubscribe;
    watch(storeName: string, keyPrefix: Uint8Array, callback: DbSubscriptionCallback): Unsubscribe;
}
