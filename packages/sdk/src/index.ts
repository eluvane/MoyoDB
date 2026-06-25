import type {
    BatchOp,
    ChangeFeed,
    ChangeFeedOptions,
    CompactionResult,
    CreateStoreOptions,
    DB,
    DbSubscriptionCallback,
    DebugFailpoint,
    ExportSnapshotOptions,
    IndexDef,
    MigrateHook,
    OpenOptions,
    PutOptions,
    Range,
    ScanItem,
    StorageInfo,
    Transaction,
    TxId,
    TxMode,
    Unsubscribe
} from './types';
import {
    acquireDbWorker,
    deleteDbByName,
    destroyDbWorker,
    invalidateTransactions,
    releaseDbWorker,
    subscribeHandleInvalidated,
    subscribeTransactionsInvalidated,
    unsafeDebugCrashWorker as crashWorkerByName,
    type RegistryEntry
} from './registry';
import {
    DatabaseBusyError,
    DatabaseClosedError,
    InvalidOpenOptionsError,
    normalizeError,
    TransactionClosedError,
    UnsupportedPlatformError,
    VersionError
} from './errors';
import {
    assertPublicStoreName,
    normalizeIndexDefinitions,
    serializeNormalizedIndexDefinitions,
    toPublicIndexDefinitions,
    type NormalizedIndexDef
} from './indexing';
import { SubscriptionHub } from './subscriptions';
import { compareStringsByCodeUnit, withTimeout } from './internal';

async function callProxy<T>(fn: () => Promise<T>): Promise<T> {
    try {
        return await fn();
    } catch (error) {
        throw normalizeError(error);
    }
}

class TransactionImpl implements Transaction {
    readonly mode: TxMode;
    #entry: RegistryEntry;
    #txId: number;
    #closed = false;
    #onClose: () => void;
    constructor(entry: RegistryEntry, txId: number, mode: TxMode, onClose: () => void) {
        this.#entry = entry;
        this.#txId = txId;
        this.mode = mode;
        this.#onClose = onClose;
    }
    async get(store: string, key: Uint8Array): Promise<Uint8Array | null> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.get(this.#txId, store, key));
    }
    async getMany(store: string, keys: Array<Uint8Array>): Promise<Array<Uint8Array | null>> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.getMany(this.#txId, store, keys));
    }
    async has(store: string, key: Uint8Array): Promise<boolean> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.has(this.#txId, store, key));
    }
    async put(store: string, key: Uint8Array, value: Uint8Array, options: PutOptions = {}): Promise<void> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.put(this.#txId, store, key, value, normalizePutOptions(options)));
    }
    async putMany(store: string, entries: Array<[Uint8Array, Uint8Array]>, options: PutOptions = {}): Promise<void> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.putMany(this.#txId, store, entries, normalizePutOptions(options)));
    }
    async delete(store: string, key: Uint8Array): Promise<boolean> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.delete(this.#txId, store, key));
    }
    async deleteMany(store: string, keys: Array<Uint8Array>): Promise<void> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.deleteMany(this.#txId, store, keys));
    }
    async applyBatch(store: string, ops: Array<BatchOp>): Promise<void> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.applyBatch(this.#txId, store, ops));
    }
    async scan(store: string, range: Range = {}): Promise<ScanItem[]> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.scan(this.#txId, store, range));
    }
    async getByIndex(store: string, indexName: string, key: Uint8Array): Promise<Uint8Array | null> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        return callProxy(() => this.#entry.proxy.getByIndex(this.#txId, store, indexName, key));
    }
    async *scanByIndex(store: string, indexName: string, range: Range = {}): AsyncIterable<[Uint8Array, Uint8Array]> {
        this.#ensureOpen();
        assertPublicStoreName(store);
        const rows = await callProxy(() => this.#entry.proxy.scanByIndex(this.#txId, store, indexName, range));
        for (const row of rows) {
            yield [row.key, row.value];
        }
    }
    async createStore(name: string, options: CreateStoreOptions = {}): Promise<void> {
        this.#ensureOpen();
        assertPublicStoreName(name);
        return callProxy(() => this.#entry.proxy.createStore(this.#txId, name, normalizeCreateStoreOptions(options)));
    }
    async dropStore(name: string): Promise<void> {
        this.#ensureOpen();
        assertPublicStoreName(name);
        return callProxy(() => this.#entry.proxy.dropStore(this.#txId, name));
    }
    async clearStore(name: string): Promise<void> {
        this.#ensureOpen();
        assertPublicStoreName(name);
        return callProxy(() => this.#entry.proxy.clearStore(this.#txId, name));
    }
    async commit(): Promise<void> {
        this.#ensureOpen();
        try {
            await callProxy(() => this.#entry.proxy.commit(this.#txId));
        } finally {
            this.#markClosed();
        }
    }
    async rollback(): Promise<void> {
        this.#ensureOpen();
        try {
            await callProxy(() => this.#entry.proxy.rollback(this.#txId));
        } finally {
            this.#markClosed();
        }
    }
    async rollbackIfOpen(): Promise<void> {
        if (this.#closed) {
            return;
        }
        if (this.#entry.invalidated) {
            this.#markClosed();
            return;
        }
        try {
            await this.#entry.proxy.rollback(this.#txId);
        } catch (error) {
            const normalized = normalizeError(error);
            if (!(normalized instanceof TransactionClosedError)) {
                throw normalized;
            }
        } finally {
            this.#markClosed();
        }
    }
    forceClose(): void {
        this.#markClosed();
    }
    internalId(): number {
        return this.#txId;
    }
    #ensureOpen() {
        if (this.#closed || this.#entry.invalidated) {
            throw new TransactionClosedError();
        }
    }
    #markClosed() {
        if (this.#closed) {
            return;
        }
        this.#closed = true;
        this.#onClose();
    }
}
class DBImpl implements DB {
    #entry: RegistryEntry;
    #closed = false;
    #transactions = new Set<TransactionImpl>();
    #subscriptions: SubscriptionHub;
    #unsubscribeTxInvalidated: (() => void) | null = null;
    #unsubscribeHandleInvalidated: (() => void) | null = null;
    constructor(entry: RegistryEntry) {
        this.#entry = entry;
        this.#subscriptions = new SubscriptionHub(entry.dbName);
        this.#unsubscribeTxInvalidated = subscribeTransactionsInvalidated(entry, () => {
            this.#forceCloseTransactions();
        });
        this.#unsubscribeHandleInvalidated = subscribeHandleInvalidated(entry, () => {
            this.#markInvalidated();
        });
    }
    async begin(mode: TxMode = 'readonly'): Promise<Transaction> {
        return this.#beginImpl(mode);
    }
    async runSchemaMigration(
        oldVersion: number,
        newVersion: number,
        migrate: MigrateHook,
        targetIndexes: IndexDef[]
    ): Promise<void> {
        this.#ensureOpen();
        const tx = await this.#beginImpl('readwrite');
        const tracker = new MigrationCatalogTracker();
        const migrationTransaction = new MigrationTransactionImpl(tx, tracker);
        const migrationDb = new MigrationDbImpl(this, migrationTransaction, tracker);
        try {
            await this.#entry.proxy.reconcileIndexes(tx.internalId(), targetIndexes);
            await migrate({
                db: migrationDb,
                oldVersion,
                newVersion,
                transaction: migrationTransaction
            });
            await this.#entry.proxy.setSchemaVersion(tx.internalId(), newVersion);
            await tx.commit();
        } catch (error) {
            try {
                await tx.rollbackIfOpen();
            } catch {}
            throw error;
        }
    }
    async listStores(): Promise<string[]> {
        this.#ensureOpen();
        return callProxy(() => this.#entry.proxy.listStores());
    }
    async getVersion(): Promise<number> {
        this.#ensureOpen();
        return callProxy(() => this.#entry.proxy.getVersion());
    }
    async changesSince(txid: TxId, options: ChangeFeedOptions = {}): Promise<ChangeFeed> {
        this.#ensureOpen();
        return callProxy(() =>
            this.#entry.proxy.changesSince(normalizeTxId(txid), normalizeChangeFeedOptions(options))
        );
    }
    async #beginImpl(mode: TxMode): Promise<TransactionImpl> {
        this.#ensureOpen();
        return callProxy(async () => {
            const txId = await this.#entry.proxy.begin(mode);
            let tx!: TransactionImpl;
            tx = new TransactionImpl(this.#entry, txId, mode, () => {
                this.#transactions.delete(tx);
            });
            this.#transactions.add(tx);
            return tx;
        });
    }
    async createStore(name: string, options: CreateStoreOptions = {}): Promise<void> {
        await this.#withScopedTx('readwrite', async (tx) => {
            await tx.createStore(name, normalizeCreateStoreOptions(options));
        });
    }
    async dropStore(name: string): Promise<void> {
        await this.#withScopedTx('readwrite', async (tx) => {
            await tx.dropStore(name);
        });
    }
    async clearStore(name: string): Promise<void> {
        await this.#withScopedTx('readwrite', async (tx) => {
            await tx.clearStore(name);
        });
    }
    async get(store: string, key: Uint8Array): Promise<Uint8Array | null> {
        return this.#withScopedTx('readonly', (tx) => tx.get(store, key));
    }
    async getMany(store: string, keys: Array<Uint8Array>): Promise<Array<Uint8Array | null>> {
        return this.#withScopedTx('readonly', (tx) => tx.getMany(store, keys));
    }
    async has(store: string, key: Uint8Array): Promise<boolean> {
        return this.#withScopedTx('readonly', (tx) => tx.has(store, key));
    }
    async put(store: string, key: Uint8Array, value: Uint8Array, options: PutOptions = {}): Promise<void> {
        await this.#withScopedTx('readwrite', async (tx) => {
            await tx.put(store, key, value, normalizePutOptions(options));
        });
    }
    async delete(store: string, key: Uint8Array): Promise<boolean> {
        return this.#withScopedTx('readwrite', (tx) => tx.delete(store, key));
    }
    async scan(store: string, range: Range = {}): Promise<ScanItem[]> {
        return this.#withScopedTx('readonly', (tx) => tx.scan(store, range));
    }
    async exportSnapshot(options: ExportSnapshotOptions = {}): Promise<Uint8Array> {
        this.#ensureOpen();
        return callProxy(async () => {
            const bytes = await this.#entry.proxy.exportSnapshot(normalizeExportSnapshotOptions(options));
            return bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
        });
    }
    async importSnapshot(data: Uint8Array): Promise<void> {
        this.#ensureOpen();
        return callProxy(() => {
            const bytes = data instanceof Uint8Array ? data : new Uint8Array(data);
            return this.#entry.proxy.importSnapshot(bytes);
        });
    }
    async reset(): Promise<void> {
        this.#ensureOpen();
        invalidateTransactions(this.#entry);
        return callProxy(() => this.#entry.proxy.reset());
    }
    async compact(): Promise<CompactionResult> {
        this.#ensureOpen();
        invalidateTransactions(this.#entry);
        return callProxy(() => this.#entry.proxy.compact());
    }
    async rebuild(): Promise<CompactionResult> {
        this.#ensureOpen();
        invalidateTransactions(this.#entry);
        return callProxy(() => this.#entry.proxy.rebuild());
    }
    async stats() {
        this.#ensureOpen();
        return callProxy(() => this.#entry.proxy.stats());
    }
    async storageInfo(): Promise<StorageInfo> {
        this.#ensureOpen();
        return callProxy(() => this.#entry.proxy.storageInfo());
    }
    async requestPersistence(): Promise<boolean> {
        this.#ensureOpen();
        return callProxy(() => this.#entry.proxy.requestPersistence());
    }
    async destroy(): Promise<void> {
        this.#ensureOpen();
        return callProxy(() => destroyDbWorker(this.#entry));
    }
    async setFailpoint(failpoint: DebugFailpoint): Promise<void> {
        this.#ensureOpen();
        return callProxy(() => this.#entry.proxy.setFailpoint(failpoint));
    }
    subscribe(callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(storeName: string, callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(storeName: string, keyPrefix: Uint8Array, callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(
        arg1: string | DbSubscriptionCallback,
        arg2?: Uint8Array | DbSubscriptionCallback,
        arg3?: DbSubscriptionCallback
    ): Unsubscribe {
        this.#ensureOpen();
        if (typeof arg1 === 'function') {
            return this.#subscriptions.subscribe(arg1);
        }
        if (typeof arg2 === 'function') {
            return this.#subscriptions.subscribe(arg1, arg2);
        }
        if (arg2 instanceof Uint8Array && typeof arg3 === 'function') {
            return this.#subscriptions.subscribe(arg1, arg2, arg3);
        }
        throw new TypeError(
            'subscribe() expects one of: (callback), (storeName, callback), (storeName, keyPrefix, callback)'
        );
    }
    watch(callback: DbSubscriptionCallback): Unsubscribe;
    watch(storeName: string, callback: DbSubscriptionCallback): Unsubscribe;
    watch(storeName: string, keyPrefix: Uint8Array, callback: DbSubscriptionCallback): Unsubscribe;
    watch(
        arg1: string | DbSubscriptionCallback,
        arg2?: Uint8Array | DbSubscriptionCallback,
        arg3?: DbSubscriptionCallback
    ): Unsubscribe {
        if (typeof arg1 === 'function') {
            return this.subscribe(arg1);
        }
        if (typeof arg2 === 'function') {
            return this.subscribe(arg1, arg2);
        }
        if (arg2 instanceof Uint8Array && typeof arg3 === 'function') {
            return this.subscribe(arg1, arg2, arg3);
        }
        throw new TypeError(
            'watch() expects one of: (callback), (storeName, callback), (storeName, keyPrefix, callback)'
        );
    }
    async close(): Promise<void> {
        if (this.#closed) {
            return;
        }
        this.#closed = true;
        this.#disposeRegistrySubscriptions();
        this.#subscriptions.close();
        let rollbackError: Error | null = null;
        const txs = Array.from(this.#transactions);
        if (txs.length > 0) {
            const results = await Promise.allSettled(txs.map((tx) => tx.rollbackIfOpen()));
            for (const result of results) {
                if (result.status === 'rejected' && rollbackError === null) {
                    rollbackError = normalizeError(result.reason);
                }
            }
        }
        try {
            await releaseDbWorker(this.#entry);
        } catch (error) {
            throw normalizeError(error);
        }
        if (rollbackError) {
            throw rollbackError;
        }
    }
    async #withScopedTx<T>(mode: TxMode, fn: (tx: Transaction) => Promise<T>): Promise<T> {
        const tx = await this.begin(mode);
        try {
            const result = await fn(tx);
            if (mode === 'readwrite') {
                await tx.commit();
            } else {
                await tx.rollback();
            }
            return result;
        } catch (error) {
            try {
                await tx.rollback();
            } catch {}
            throw error;
        }
    }
    #disposeRegistrySubscriptions() {
        this.#unsubscribeTxInvalidated?.();
        this.#unsubscribeTxInvalidated = null;
        this.#unsubscribeHandleInvalidated?.();
        this.#unsubscribeHandleInvalidated = null;
    }
    #forceCloseTransactions() {
        for (const tx of Array.from(this.#transactions)) {
            tx.forceClose();
        }
    }
    #markInvalidated() {
        if (this.#closed) {
            return;
        }
        this.#closed = true;
        this.#disposeRegistrySubscriptions();
        this.#forceCloseTransactions();
        this.#subscriptions.close();
    }
    #ensureOpen() {
        if (this.#closed || this.#entry.invalidated) {
            throw new DatabaseClosedError();
        }
    }
}
class MigrationCatalogTracker {
    #created = new Set<string>();
    #dropped = new Set<string>();
    recordCreate(name: string): void {
        this.#dropped.delete(name);
        this.#created.add(name);
    }
    recordDrop(name: string): void {
        if (this.#created.delete(name)) {
            return;
        }
        this.#dropped.add(name);
    }
    apply(stores: string[]): string[] {
        const visible = new Set(stores);
        for (const name of this.#created) {
            visible.add(name);
        }
        for (const name of this.#dropped) {
            visible.delete(name);
        }
        return Array.from(visible).sort(compareStringsByCodeUnit);
    }
}
class MigrationTransactionImpl implements Transaction {
    #inner: TransactionImpl;
    #tracker: MigrationCatalogTracker;
    constructor(inner: TransactionImpl, tracker: MigrationCatalogTracker) {
        this.#inner = inner;
        this.#tracker = tracker;
    }
    get mode(): TxMode {
        return this.#inner.mode;
    }
    async get(store: string, key: Uint8Array): Promise<Uint8Array | null> {
        return this.#inner.get(store, key);
    }
    async getMany(store: string, keys: Array<Uint8Array>): Promise<Array<Uint8Array | null>> {
        return this.#inner.getMany(store, keys);
    }
    async has(store: string, key: Uint8Array): Promise<boolean> {
        return this.#inner.has(store, key);
    }
    async put(store: string, key: Uint8Array, value: Uint8Array, options: PutOptions = {}): Promise<void> {
        await this.#inner.put(store, key, value, options);
    }
    async putMany(store: string, entries: Array<[Uint8Array, Uint8Array]>, options: PutOptions = {}): Promise<void> {
        await this.#inner.putMany(store, entries, options);
    }
    async delete(store: string, key: Uint8Array): Promise<boolean> {
        return this.#inner.delete(store, key);
    }
    async deleteMany(store: string, keys: Array<Uint8Array>): Promise<void> {
        await this.#inner.deleteMany(store, keys);
    }
    async applyBatch(store: string, ops: Array<BatchOp>): Promise<void> {
        await this.#inner.applyBatch(store, ops);
    }
    async scan(store: string, range: Range = {}): Promise<ScanItem[]> {
        return this.#inner.scan(store, range);
    }
    async getByIndex(store: string, indexName: string, key: Uint8Array): Promise<Uint8Array | null> {
        return this.#inner.getByIndex(store, indexName, key);
    }
    scanByIndex(store: string, indexName: string, range: Range = {}): AsyncIterable<[Uint8Array, Uint8Array]> {
        return this.#inner.scanByIndex(store, indexName, range);
    }
    async createStore(name: string, options: CreateStoreOptions = {}): Promise<void> {
        await this.#inner.createStore(name, options);
        this.#tracker.recordCreate(name);
    }
    async dropStore(name: string): Promise<void> {
        await this.#inner.dropStore(name);
        this.#tracker.recordDrop(name);
    }
    async clearStore(name: string): Promise<void> {
        await this.#inner.clearStore(name);
    }
    async commit(): Promise<void> {
        throw migrationUnsupportedMethod('transaction.commit');
    }
    async rollback(): Promise<void> {
        throw migrationUnsupportedMethod('transaction.rollback');
    }
}
class MigrationDbImpl implements DB {
    #db: DBImpl;
    #transaction: MigrationTransactionImpl;
    #tracker: MigrationCatalogTracker;
    constructor(db: DBImpl, transaction: MigrationTransactionImpl, tracker: MigrationCatalogTracker) {
        this.#db = db;
        this.#transaction = transaction;
        this.#tracker = tracker;
    }
    async begin(_mode: TxMode = 'readonly'): Promise<Transaction> {
        throw migrationUnsupportedMethod('db.begin');
    }
    async createStore(name: string, options: CreateStoreOptions = {}): Promise<void> {
        await this.#transaction.createStore(name, options);
    }
    async dropStore(name: string): Promise<void> {
        await this.#transaction.dropStore(name);
    }
    async clearStore(name: string): Promise<void> {
        await this.#transaction.clearStore(name);
    }
    async listStores(): Promise<string[]> {
        return this.#tracker.apply(await this.#db.listStores());
    }
    async getVersion(): Promise<number> {
        return this.#db.getVersion();
    }
    async changesSince(_txid: TxId, _options: ChangeFeedOptions = {}): Promise<ChangeFeed> {
        throw migrationUnsupportedMethod('db.changesSince');
    }
    async get(store: string, key: Uint8Array): Promise<Uint8Array | null> {
        return this.#transaction.get(store, key);
    }
    async getMany(store: string, keys: Array<Uint8Array>): Promise<Array<Uint8Array | null>> {
        return this.#transaction.getMany(store, keys);
    }
    async has(store: string, key: Uint8Array): Promise<boolean> {
        return this.#transaction.has(store, key);
    }
    async put(store: string, key: Uint8Array, value: Uint8Array, options: PutOptions = {}): Promise<void> {
        await this.#transaction.put(store, key, value, options);
    }
    async delete(store: string, key: Uint8Array): Promise<boolean> {
        return this.#transaction.delete(store, key);
    }
    async scan(store: string, range: Range = {}): Promise<ScanItem[]> {
        return this.#transaction.scan(store, range);
    }
    async exportSnapshot(_options: ExportSnapshotOptions = {}): Promise<Uint8Array> {
        throw migrationUnsupportedMethod('db.exportSnapshot');
    }
    async importSnapshot(_data: Uint8Array): Promise<void> {
        throw migrationUnsupportedMethod('db.importSnapshot');
    }
    async reset(): Promise<void> {
        throw migrationUnsupportedMethod('db.reset');
    }
    async compact(): Promise<CompactionResult> {
        throw migrationUnsupportedMethod('db.compact');
    }
    async rebuild(): Promise<CompactionResult> {
        throw migrationUnsupportedMethod('db.rebuild');
    }
    async stats() {
        return this.#db.stats();
    }
    async storageInfo(): Promise<StorageInfo> {
        return this.#db.storageInfo();
    }
    async requestPersistence(): Promise<boolean> {
        return this.#db.requestPersistence();
    }
    async close(): Promise<void> {
        throw migrationUnsupportedMethod('db.close');
    }
    async destroy(): Promise<void> {
        throw migrationUnsupportedMethod('db.destroy');
    }
    async setFailpoint(_failpoint: DebugFailpoint): Promise<void> {
        throw migrationUnsupportedMethod('db.setFailpoint');
    }
    subscribe(callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(storeName: string, callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(storeName: string, keyPrefix: Uint8Array, callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(
        _arg1: string | DbSubscriptionCallback,
        _arg2?: Uint8Array | DbSubscriptionCallback,
        _arg3?: DbSubscriptionCallback
    ): Unsubscribe {
        throw migrationUnsupportedMethod('db.subscribe');
    }
    watch(callback: DbSubscriptionCallback): Unsubscribe;
    watch(storeName: string, callback: DbSubscriptionCallback): Unsubscribe;
    watch(storeName: string, keyPrefix: Uint8Array, callback: DbSubscriptionCallback): Unsubscribe;
    watch(
        _arg1: string | DbSubscriptionCallback,
        _arg2?: Uint8Array | DbSubscriptionCallback,
        _arg3?: DbSubscriptionCallback
    ): Unsubscribe {
        throw migrationUnsupportedMethod('db.watch');
    }
}
function migrationUnsupportedMethod(method: string): Error {
    const error = new Error(
        `${method}() is not available inside openDB() migrations; use the provided migration transaction and wait for openDB() to resolve`
    );
    error.name = 'InvalidStateError';
    return error;
}
function normalizeDatabaseName(value: unknown, method: 'openDB' | 'deleteDB' | 'unsafeDebugCrashWorker'): string {
    if (typeof value !== 'string' || value.length === 0) {
        throw new TypeError(`${method}() database name must be a non-empty string`);
    }
    return value;
}
type ErrorFactory = (message: string) => Error;

const typeError: ErrorFactory = (message) => new TypeError(message);
const invalidOpenOptionsError: ErrorFactory = (message) => new InvalidOpenOptionsError(message);

function normalizeOptionsObject<T>(options: unknown, message: string, createError: ErrorFactory = typeError): T {
    if (options === undefined) {
        return {} as T;
    }
    if (options === null || typeof options !== 'object' || Array.isArray(options)) {
        throw createError(message);
    }
    return options as T;
}

function normalizeNonNegativeSafeInteger(
    value: unknown,
    message: string,
    createError: ErrorFactory = typeError
): number {
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
        throw createError(message);
    }
    return value;
}

function normalizeOptionalNonNegativeSafeInteger(
    value: unknown,
    message: string,
    createError?: ErrorFactory
): number | undefined {
    return value === undefined ? undefined : normalizeNonNegativeSafeInteger(value, message, createError);
}

function normalizeOpenOptionsInput(options: unknown): OpenOptions {
    return normalizeOptionsObject(options, 'open options must be an object', invalidOpenOptionsError);
}
function normalizeSchemaVersion(value: unknown): number | undefined {
    return normalizeOptionalNonNegativeSafeInteger(
        value,
        'version must be a non-negative safe integer',
        invalidOpenOptionsError
    );
}
function normalizePutOptions(options: unknown): PutOptions {
    const { ttl } = normalizeOptionsObject<{
        ttl?: unknown;
    }>(options, 'put options must be an object');
    const normalizedTtl = normalizeOptionalNonNegativeSafeInteger(ttl, 'ttl must be a non-negative safe integer');
    return normalizedTtl === undefined ? {} : { ttl: normalizedTtl };
}
function normalizeCompressionOptions<T extends CreateStoreOptions | ExportSnapshotOptions>(
    options: unknown,
    message: string
): T {
    const { compression } = normalizeOptionsObject<{
        compression?: unknown;
    }>(options, message);
    if (compression === undefined) {
        return {} as T;
    }
    if (compression !== false && compression !== 'gzip' && compression !== 'deflate') {
        throw new TypeError('compression must be "gzip", "deflate", or false');
    }
    return { compression } as T;
}

function normalizeCreateStoreOptions(options: unknown): CreateStoreOptions {
    return normalizeCompressionOptions(options, 'createStore options must be an object');
}

function normalizeExportSnapshotOptions(options: unknown): ExportSnapshotOptions {
    return normalizeCompressionOptions(options, 'exportSnapshot options must be an object');
}
function normalizeTxId(value: unknown): TxId {
    return normalizeNonNegativeSafeInteger(value, 'txid must be a non-negative safe integer');
}
function normalizeChangeFeedOptions(options: unknown): ChangeFeedOptions {
    const { stores, limit } = normalizeOptionsObject<{
        stores?: unknown;
        limit?: unknown;
    }>(options, 'change feed options must be an object');
    const normalized: ChangeFeedOptions = {};
    if (stores !== undefined) {
        if (!Array.isArray(stores) || stores.some((store) => typeof store !== 'string')) {
            throw new TypeError('stores must be an array of strings');
        }
        for (const store of stores) {
            assertPublicStoreName(store);
        }
        normalized.stores = [...stores];
    }
    const normalizedLimit = normalizeOptionalNonNegativeSafeInteger(limit, 'limit must be a non-negative safe integer');
    if (normalizedLimit !== undefined) {
        normalized.limit = normalizedLimit;
    }
    return normalized;
}
function normalizeMigrateHook(value: unknown): MigrateHook | undefined {
    if (value === undefined) {
        return undefined;
    }
    if (typeof value !== 'function') {
        throw new InvalidOpenOptionsError('migrate must be a function');
    }
    return value as MigrateHook;
}
function normalizeRequestedIndexes(value: unknown): NormalizedIndexDef[] | undefined {
    if (value === undefined) {
        return undefined;
    }
    return normalizeIndexDefinitions(value);
}
function indexDefinitionsMatch(
    left: ReadonlyArray<NormalizedIndexDef>,
    right: ReadonlyArray<NormalizedIndexDef>
): boolean {
    return serializeNormalizedIndexDefinitions(left) === serializeNormalizedIndexDefinitions(right);
}
async function loadNormalizedIndexes(entry: RegistryEntry): Promise<NormalizedIndexDef[]> {
    return normalizeIndexDefinitions(await entry.proxy.getIndexes());
}
function assertMainThreadCapabilities() {
    if (!globalThis.isSecureContext) {
        throw new UnsupportedPlatformError('moyodb requires a secure context (HTTPS)');
    }
    const nav = globalThis.navigator;
    if (!nav?.storage) {
        throw new UnsupportedPlatformError('navigator.storage is unavailable');
    }
    if (typeof nav.storage.getDirectory !== 'function') {
        throw new UnsupportedPlatformError('navigator.storage.getDirectory is unavailable');
    }
    if (typeof globalThis.Worker !== 'function') {
        throw new UnsupportedPlatformError('Worker is unavailable');
    }
    if (typeof globalThis.BroadcastChannel === 'undefined') {
        throw new UnsupportedPlatformError('BroadcastChannel is unavailable');
    }
    if (!nav.locks) {
        throw new UnsupportedPlatformError('navigator.locks is unavailable');
    }
}
async function requestPersistentStorageOnOpen(options: OpenOptions): Promise<void> {
    const nav = globalThis.navigator;
    if ((options.requestPersistence ?? true) && typeof nav?.storage?.persist === 'function') {
        await withTimeout(nav.storage.persist(), 1000, false);
    }
}
export async function openDB(name: string, options: OpenOptions = {}): Promise<DB> {
    const dbName = normalizeDatabaseName(name, 'openDB');
    const normalizedOptions = normalizeOpenOptionsInput(options);
    const requestedVersion = normalizeSchemaVersion(normalizedOptions.version);
    const migrate = normalizeMigrateHook(normalizedOptions.migrate);
    const requestedIndexes = normalizeRequestedIndexes(normalizedOptions.indexes);
    if (requestedVersion === undefined && migrate !== undefined) {
        throw new InvalidOpenOptionsError('migrate requires version');
    }
    if (requestedVersion === undefined && requestedIndexes !== undefined) {
        throw new InvalidOpenOptionsError('indexes requires version');
    }
    assertMainThreadCapabilities();
    await requestPersistentStorageOnOpen(normalizedOptions);
    const entry = await acquireDbWorker(dbName, normalizedOptions);
    const db = new DBImpl(entry);
    let keepHandle = false;
    try {
        if (requestedVersion === undefined) {
            keepHandle = true;
            return db;
        }
        const [currentVersion, currentIndexes] = await Promise.all([db.getVersion(), loadNormalizedIndexes(entry)]);
        if (requestedVersion < currentVersion) {
            throw new VersionError(
                `cannot open database ${dbName} at schema version ${requestedVersion}; current schema version is ${currentVersion}`
            );
        }
        if (requestedVersion === currentVersion) {
            if (requestedIndexes !== undefined && !indexDefinitionsMatch(requestedIndexes, currentIndexes)) {
                throw new VersionError(
                    `database ${dbName} is already at schema version ${currentVersion}, but the requested index catalog does not match the committed schema`
                );
            }
            keepHandle = true;
            return db;
        }
        if (entry.refs > 1) {
            throw new DatabaseBusyError(
                `database ${dbName} is already open in this tab; close existing handles before migrating from version ${currentVersion} to ${requestedVersion}`
            );
        }
        if (!migrate) {
            throw new VersionError(
                `database ${dbName} requires migration from version ${currentVersion} to ${requestedVersion}, but no migrate hook was provided`
            );
        }
        const targetIndexes = requestedIndexes ?? currentIndexes;
        await db.runSchemaMigration(currentVersion, requestedVersion, migrate, toPublicIndexDefinitions(targetIndexes));
        keepHandle = true;
        return db;
    } catch (error) {
        if (!keepHandle) {
            try {
                await db.close();
            } catch {}
        }
        throw normalizeError(error);
    }
}
export async function deleteDB(name: string): Promise<void> {
    const dbName = normalizeDatabaseName(name, 'deleteDB');
    assertMainThreadCapabilities();
    await deleteDbByName(dbName);
}
export function unsafeDebugCrashWorker(name: string): boolean {
    const dbName = normalizeDatabaseName(name, 'unsafeDebugCrashWorker');
    return crashWorkerByName(dbName);
}
export * from './codec';
export * from './errors';
export * from './indexing';
export type * from './types';
