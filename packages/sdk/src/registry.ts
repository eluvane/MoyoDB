import { WorkerProtocolClient } from './worker-client';
import type { DebugFailpoint, OpenOptions } from './types';
import { InvalidOpenOptionsError, normalizeError } from './errors';
import { withTimeout } from './internal';
const VALID_FAILPOINTS = new Set<Exclude<DebugFailpoint, null>>([
    'after_wal_flush',
    'after_main_flush',
    'before_superblock_flush'
]);
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
interface MainThreadPersistenceBridge {
    close(): void;
}
export interface NormalizedOpenOptions {
    createIfMissing: boolean;
    ownerWaitMs: number;
    requestPersistence: boolean;
    cachePages: number;
    debugFailpoint: DebugFailpoint;
}
export interface RegistryEntry {
    dbName: string;
    refs: number;
    worker: Worker;
    proxy: WorkerProtocolClient;
    persistenceBridge: MainThreadPersistenceBridge;
    options: NormalizedOpenOptions;
    invalidated: boolean;
    txInvalidationListeners: Set<() => void>;
    handleInvalidationListeners: Set<() => void>;
}
const registry = new Map<string, RegistryEntry>();
function requirePlainOptionsObject(options: OpenOptions): asserts options is OpenOptions {
    if (options === null || typeof options !== 'object' || Array.isArray(options)) {
        throw new InvalidOpenOptionsError('open options must be an object');
    }
}
function normalizeBoolean(value: unknown, field: string, defaultValue: boolean): boolean {
    if (value === undefined) {
        return defaultValue;
    }
    if (typeof value !== 'boolean') {
        throw new InvalidOpenOptionsError(`${field} must be a boolean`);
    }
    return value;
}
function normalizeNonNegativeInteger(value: unknown, field: string, defaultValue: number): number {
    if (value === undefined) {
        return defaultValue;
    }
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
        throw new InvalidOpenOptionsError(`${field} must be a non-negative safe integer`);
    }
    return value;
}
function normalizeCachePages(value: unknown): number {
    return Math.max(1, normalizeNonNegativeInteger(value, 'cachePages', 256));
}
function normalizeFailpoint(value: unknown): DebugFailpoint {
    if (value === undefined || value === null) {
        return null;
    }
    if (typeof value !== 'string' || !VALID_FAILPOINTS.has(value as Exclude<DebugFailpoint, null>)) {
        throw new InvalidOpenOptionsError(`debugFailpoint must be one of: ${Array.from(VALID_FAILPOINTS).join(', ')}`);
    }
    return value as Exclude<DebugFailpoint, null>;
}
function normalizeOptions(options: OpenOptions = {}): NormalizedOpenOptions {
    requirePlainOptionsObject(options);
    return {
        createIfMissing: normalizeBoolean(options.createIfMissing, 'createIfMissing', true),
        ownerWaitMs: normalizeNonNegativeInteger(options.ownerWaitMs, 'ownerWaitMs', 0),
        requestPersistence: normalizeBoolean(options.requestPersistence, 'requestPersistence', true),
        cachePages: normalizeCachePages(options.cachePages),
        debugFailpoint: normalizeFailpoint(options.debugFailpoint)
    };
}
function assertCompatibleOptions(dbName: string, current: NormalizedOpenOptions, next: NormalizedOpenOptions) {
    if (current.cachePages !== next.cachePages) {
        throw new InvalidOpenOptionsError(
            `database ${dbName} is already open in this tab with cachePages=${current.cachePages}; requested cachePages=${next.cachePages}`
        );
    }
}
async function queryPersistentStorageState(): Promise<boolean> {
    const storage = globalThis.navigator?.storage;
    if (typeof storage?.persisted !== 'function') {
        return false;
    }
    return await withTimeout(storage.persisted(), 1000, false);
}
async function requestPersistentStorageGrant(): Promise<boolean> {
    const storage = globalThis.navigator?.storage;
    if (typeof storage?.persist !== 'function') {
        return false;
    }
    return await withTimeout(storage.persist(), 1000, false);
}
async function handlePersistenceBridgeRequest(port: MessagePort, request: PersistenceBridgeRequest): Promise<void> {
    const granted =
        request.op === 'persist' ? await requestPersistentStorageGrant() : await queryPersistentStorageState();
    const response: PersistenceBridgeResponse = {
        type: PERSISTENCE_BRIDGE_RESPONSE,
        id: request.id,
        granted
    };
    port.postMessage(response);
}
function createMainThreadPersistenceBridge(worker: Worker): MainThreadPersistenceBridge {
    const channel = new MessageChannel();
    const port = channel.port1;
    port.addEventListener('message', (event: MessageEvent<PersistenceBridgeRequest>) => {
        const data = event.data;
        if (!data || data.type !== PERSISTENCE_BRIDGE_REQUEST) {
            return;
        }
        void handlePersistenceBridgeRequest(port, data);
    });
    port.start();
    worker.postMessage({ type: PERSISTENCE_BRIDGE_INIT }, [channel.port2]);
    return {
        close() {
            port.close();
        }
    };
}
function createWorker(): {
    worker: Worker;
    proxy: WorkerProtocolClient;
    persistenceBridge: MainThreadPersistenceBridge;
} {
    const worker = new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' });
    const proxy = new WorkerProtocolClient(worker);
    const persistenceBridge = createMainThreadPersistenceBridge(worker);
    return { worker, proxy, persistenceBridge };
}
function createEntry(
    dbName: string,
    worker: Worker,
    proxy: WorkerProtocolClient,
    persistenceBridge: MainThreadPersistenceBridge,
    options: NormalizedOpenOptions
): RegistryEntry {
    const entry: RegistryEntry = {
        dbName,
        refs: 1,
        worker,
        proxy,
        persistenceBridge,
        options,
        invalidated: false,
        txInvalidationListeners: new Set(),
        handleInvalidationListeners: new Set()
    };
    proxy.setFatalHandler(() => {
        const current = registry.get(dbName);
        if (current !== entry || entry.invalidated) {
            return;
        }
        registry.delete(dbName);
        entry.refs = 0;
        invalidateEntry(entry);
        entry.persistenceBridge.close();
        entry.worker.terminate();
    });
    return entry;
}
function emitListeners(listeners: Iterable<() => void>) {
    for (const listener of Array.from(listeners)) {
        listener();
    }
}
export function subscribeTransactionsInvalidated(entry: RegistryEntry, listener: () => void): () => void {
    entry.txInvalidationListeners.add(listener);
    return () => {
        entry.txInvalidationListeners.delete(listener);
    };
}
export function subscribeHandleInvalidated(entry: RegistryEntry, listener: () => void): () => void {
    entry.handleInvalidationListeners.add(listener);
    return () => {
        entry.handleInvalidationListeners.delete(listener);
    };
}
export function invalidateTransactions(entry: RegistryEntry): void {
    emitListeners(entry.txInvalidationListeners);
}
function invalidateEntry(entry: RegistryEntry): void {
    if (entry.invalidated) {
        return;
    }
    entry.invalidated = true;
    invalidateTransactions(entry);
    emitListeners(entry.handleInvalidationListeners);
}
export function unsafeDebugCrashWorker(dbName: string): boolean {
    const current = registry.get(dbName);
    if (!current) {
        return false;
    }
    registry.delete(dbName);
    invalidateEntry(current);
    current.refs = 0;
    current.persistenceBridge.close();
    current.proxy.dispose(new Error('worker was terminated by unsafeDebugCrashWorker'));
    current.worker.terminate();
    return true;
}
export async function acquireDbWorker(dbName: string, options: OpenOptions = {}): Promise<RegistryEntry> {
    const normalized = normalizeOptions(options);
    const existing = registry.get(dbName);
    if (existing) {
        if (existing.invalidated) {
            registry.delete(dbName);
        } else {
            assertCompatibleOptions(dbName, existing.options, normalized);
            if (normalized.debugFailpoint !== null) {
                try {
                    await existing.proxy.setFailpoint(normalized.debugFailpoint);
                } catch (error) {
                    throw normalizeError(error);
                }
            }
            existing.refs += 1;
            return existing;
        }
    }
    const { worker, proxy, persistenceBridge } = createWorker();
    const entry = createEntry(dbName, worker, proxy, persistenceBridge, normalized);
    try {
        await proxy.open({
            dbName,
            options: normalized
        });
        registry.set(dbName, entry);
        return entry;
    } catch (error) {
        persistenceBridge.close();
        proxy.dispose(new Error('worker open failed'));
        worker.terminate();
        throw normalizeError(error);
    }
}
export async function releaseDbWorker(entry: RegistryEntry): Promise<void> {
    if (entry.invalidated) {
        return;
    }
    const current = registry.get(entry.dbName);
    if (!current) {
        return;
    }
    current.refs -= 1;
    if (current.refs > 0) {
        return;
    }
    registry.delete(entry.dbName);
    try {
        await current.proxy.close();
    } finally {
        current.persistenceBridge.close();
        current.proxy.dispose(new Error('worker was released'));
        current.worker.terminate();
    }
}
export async function destroyDbWorker(entry: RegistryEntry): Promise<void> {
    const current = registry.get(entry.dbName) ?? entry;
    registry.delete(entry.dbName);
    invalidateEntry(current);
    current.refs = 0;
    try {
        await current.proxy.destroy();
    } catch (error) {
        throw normalizeError(error);
    } finally {
        current.persistenceBridge.close();
        current.proxy.dispose(new Error('worker was released'));
        current.worker.terminate();
    }
}
export async function deleteDbByName(dbName: string): Promise<void> {
    const current = registry.get(dbName);
    if (current) {
        await destroyDbWorker(current);
        return;
    }
    const { worker, proxy, persistenceBridge } = createWorker();
    try {
        await proxy.deleteDB(dbName);
    } catch (error) {
        throw normalizeError(error);
    } finally {
        persistenceBridge.close();
        proxy.dispose(new Error('temporary worker was terminated'));
        worker.terminate();
    }
}
