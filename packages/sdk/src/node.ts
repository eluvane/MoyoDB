import { createHash, randomUUID } from 'node:crypto';
import { realpath } from 'node:fs/promises';
import { join } from 'node:path';
import { openDBWithWorker } from './index';
import { assertCompatibleOptions, invalidateTransactions, normalizeOptions, type RegistryEntry } from './registry';
import {
    DatabaseBusyError,
    DatabaseClosedError,
    InvalidOpenOptionsError,
    StorageError,
    normalizeError
} from './errors';
import { NodeWorkerTransport } from './node-worker-client';
import { MISSING_DATABASE_MESSAGE, ensureStorageRoot, releaseNodeStorageLease } from './node-storage.mjs';
import { workerProtocolError } from './worker-protocol';
import type { DB, OpenOptions } from './types';

export type NodeOpenOptions = Omit<OpenOptions, 'workerMode'> & { directory: string };
export interface NodeDeleteOptions {
    directory: string;
}

interface NodeRegistryEntry extends RegistryEntry {
    directory: string;
    registryKey: string;
    shutdown: Promise<void> | null;
}

const registry = new Map<string, NodeRegistryEntry>();
const lifecycleOperations = new Map<string, Promise<void>>();

function queueLifecycleOperation<T>(key: string, operation: () => Promise<T>): Promise<T> {
    const previous = lifecycleOperations.get(key) ?? Promise.resolve();
    const result = previous.then(operation);
    const settled = result.then(
        () => undefined,
        () => undefined
    );
    lifecycleOperations.set(key, settled);
    void settled.then(() => {
        if (lifecycleOperations.get(key) === settled) lifecycleOperations.delete(key);
    });
    return result;
}

function validateName(name: unknown, method: string): asserts name is string {
    if (typeof name !== 'string' || name.length === 0) {
        throw new TypeError(`${method}() database name must be a non-empty string`);
    }
    if (/[\uD800-\uDFFF]/u.test(name)) {
        throw new TypeError(`${method}() database name must contain valid Unicode`);
    }
    if (Buffer.byteLength(name, 'utf8') > 127) {
        throw new TypeError(`${method}() database name must contain at most 127 UTF-8 bytes`);
    }
}

function validateDirectoryOptions(options: unknown): asserts options is NodeDeleteOptions {
    if (options === null || typeof options !== 'object' || Array.isArray(options)) {
        throw new InvalidOpenOptionsError('Node options must contain directory');
    }
    const directory = (options as NodeDeleteOptions).directory;
    if (typeof directory !== 'string' || directory.length === 0 || directory.includes('\0')) {
        throw new InvalidOpenOptionsError('directory must be a non-empty filesystem path');
    }
}

async function databaseLocation(
    name: string,
    options: NodeDeleteOptions & { createIfMissing?: boolean }
): Promise<{
    directory: string;
    registryKey: string;
}> {
    try {
        ensureStorageRoot(options.directory, options.createIfMissing !== false);
    } catch (error) {
        if (error instanceof Error && error.name === 'StorageError') {
            throw new StorageError(error.message || MISSING_DATABASE_MESSAGE);
        }
        throw error;
    }
    const root = await realpath(options.directory);
    const directory = join(root, 'stackdb', Buffer.from(name, 'utf8').toString('hex'));
    const registryKey = process.platform === 'win32' ? directory.toLowerCase() : directory;
    return { directory, registryKey };
}

function invalidateEntry(entry: NodeRegistryEntry): void {
    if (entry.invalidated) return;
    entry.invalidated = true;
    entry.refs = 0;
    invalidateTransactions(entry);
    for (const listener of Array.from(entry.handleInvalidationListeners)) listener();
}

function shutdownEntry(entry: NodeRegistryEntry, reason: Error): Promise<void> {
    if (entry.shutdown) return entry.shutdown;
    entry.proxy.dispose(reason);
    entry.shutdown = Promise.resolve(entry.worker.terminate()).then(() => undefined);
    return entry.shutdown;
}

function createEntry(dbName: string, directory: string, registryKey: string, options: OpenOptions): NodeRegistryEntry {
    const normalized = normalizeOptions({ ...options, workerMode: 'dedicated' });
    const lockToken = randomUUID();
    const channelName = `node:${createHash('sha256').update(registryKey).digest('hex')}`;
    const worker = new NodeWorkerTransport(
        {
            directory,
            dbName,
            channelName,
            lockToken,
            ownerWaitMs: normalized.ownerWaitMs,
            encodedDbName: Buffer.from(dbName, 'utf8').toString('hex'),
            createIfMissing: normalized.createIfMissing
        },
        () => {
            releaseNodeStorageLease(directory, lockToken);
        }
    );
    const entry: NodeRegistryEntry = {
        dbName,
        directory,
        registryKey,
        worker,
        proxy: worker.proxy,
        persistenceBridge: { close() {} },
        options: normalized,
        refs: 1,
        invalidated: false,
        schemaMigrationInProgress: false,
        txInvalidationListeners: new Set(),
        handleInvalidationListeners: new Set(),
        channelName,
        shutdown: null,
        release: () => releaseEntry(entry),
        destroy: () => queueLifecycleOperation(registryKey, () => destroyEntry(entry))
    };
    worker.proxy.setFatalHandler((error) => {
        if (registry.get(registryKey) === entry) registry.delete(registryKey);
        invalidateEntry(entry);
        void queueLifecycleOperation(registryKey, () => shutdownEntry(entry, error)).catch(() => {});
    });
    return entry;
}

async function acquireEntry(
    dbName: string,
    directory: string,
    registryKey: string,
    options: OpenOptions
): Promise<RegistryEntry> {
    const normalized = normalizeOptions({ ...options, workerMode: 'dedicated' });
    return queueLifecycleOperation(registryKey, async () => {
        const existing = registry.get(registryKey);
        if (existing && !existing.invalidated) {
            if (existing.schemaMigrationInProgress) {
                throw new DatabaseBusyError(`database ${dbName} is migrating; wait for openNodeDB() to resolve`);
            }
            assertCompatibleOptions(dbName, existing.options, normalized);
            if (normalized.debugFailpoint !== null) await existing.proxy.setFailpoint(normalized.debugFailpoint);
            existing.refs += 1;
            return existing;
        }
        const entry = createEntry(dbName, directory, registryKey, options);
        try {
            await entry.proxy.open({ dbName, options: normalized });
            if (entry.invalidated) throw new DatabaseClosedError();
            registry.set(registryKey, entry);
            return entry;
        } catch (error) {
            await shutdownEntry(entry, workerProtocolError('WorkerTerminatedError', 'Node worker open failed'));
            throw normalizeError(error);
        }
    });
}

async function releaseEntry(entry: NodeRegistryEntry): Promise<void> {
    return queueLifecycleOperation(entry.registryKey, async () => {
        if (entry.invalidated || registry.get(entry.registryKey) !== entry) {
            if (entry.shutdown) await entry.shutdown;
            return;
        }
        entry.refs -= 1;
        if (entry.refs > 0) return;
        registry.delete(entry.registryKey);
        try {
            await entry.proxy.close();
        } finally {
            invalidateEntry(entry);
            await shutdownEntry(entry, workerProtocolError('WorkerTerminatedError', 'Node worker was released'));
        }
    });
}

async function destroyEntry(entry: NodeRegistryEntry): Promise<void> {
    if (entry.invalidated || registry.get(entry.registryKey) !== entry) throw new DatabaseClosedError();
    registry.delete(entry.registryKey);
    invalidateEntry(entry);
    try {
        await entry.proxy.destroy();
    } finally {
        await shutdownEntry(entry, workerProtocolError('WorkerTerminatedError', 'Node database was destroyed'));
    }
}

export async function openNodeDB(name: string, options: NodeOpenOptions): Promise<DB> {
    validateName(name, 'openNodeDB');
    validateDirectoryOptions(options);
    const { directory, registryKey } = await databaseLocation(name, options);
    return openDBWithWorker(name, options, (dbName, openOptions) =>
        acquireEntry(dbName, directory, registryKey, openOptions)
    );
}

export async function deleteNodeDB(name: string, options: NodeDeleteOptions): Promise<void> {
    validateName(name, 'deleteNodeDB');
    validateDirectoryOptions(options);
    const { directory, registryKey } = await databaseLocation(name, options);
    return queueLifecycleOperation(registryKey, async () => {
        const existing = registry.get(registryKey);
        if (existing) return destroyEntry(existing);
        const entry = createEntry(name, directory, registryKey, {});
        try {
            await entry.proxy.deleteDB(name);
        } finally {
            await shutdownEntry(entry, workerProtocolError('WorkerTerminatedError', 'Node delete worker was released'));
        }
    });
}

export async function unsafeDebugCrashNodeWorker(name: string, options: NodeDeleteOptions): Promise<boolean> {
    validateName(name, 'unsafeDebugCrashNodeWorker');
    validateDirectoryOptions(options);
    const { registryKey } = await databaseLocation(name, options);
    return queueLifecycleOperation(registryKey, async () => {
        const entry = registry.get(registryKey);
        if (!entry) return false;
        registry.delete(registryKey);
        invalidateEntry(entry);
        await shutdownEntry(entry, workerProtocolError('WorkerTerminatedError', 'Node worker was terminated'));
        return true;
    });
}

export * from './codec';
export * from './errors';
export * from './indexing';
export * from './records';
export * from './sql';
export { SqlSyntaxError } from './sql-parser';
export type * from './sql-types';
export type * from './types';
