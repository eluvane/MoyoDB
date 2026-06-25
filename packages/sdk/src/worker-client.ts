import type { WorkerApi, WorkerOpenRequest } from './worker-api';
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
import {
    WORKER_PROTOCOL_REQUEST,
    WORKER_PROTOCOL_VERSION,
    deserializeWorkerError,
    decodeWorkerResponsePayload,
    isWorkerProtocolReadyMessage,
    isWorkerProtocolResponseMessage,
    prepareWorkerCommandPayload,
    workerProtocolError,
    type WorkerCommand,
    type WorkerCommandArgs,
    type WorkerCommandResult,
    type WorkerProtocolRequestMessage
} from './worker-protocol';

interface PendingRequest {
    command: WorkerCommand;
    resolve: (value: unknown) => void;
    reject: (error: Error) => void;
    timeout: ReturnType<typeof setTimeout> | null;
}

function clearPendingTimeout(pending: PendingRequest): void {
    if (pending.timeout) {
        clearTimeout(pending.timeout);
    }
}

export interface WorkerProtocolClientOptions {
    requestTimeoutMs?: number;
}

export class WorkerProtocolClient implements WorkerApi {
    private nextRequestId = 1;
    private pending = new Map<number, PendingRequest>();
    private closed = false;
    private readyResolved = false;
    private readyResolve: (() => void) | null = null;
    private readyReject: ((error: Error) => void) | null = null;
    private ready: Promise<void>;
    private requestTimeoutMs: number;
    private fatalHandler: ((error: Error) => void) | null = null;
    private closeReason: Error | null = null;

    constructor(
        private readonly worker: Worker,
        options: WorkerProtocolClientOptions = {}
    ) {
        this.requestTimeoutMs = options.requestTimeoutMs ?? 0;
        this.ready = new Promise<void>((resolve, reject) => {
            this.readyResolve = resolve;
            this.readyReject = reject;
        });
        this.worker.addEventListener('message', this.handleMessage);
        this.worker.addEventListener('messageerror', this.handleMessageError);
        this.worker.addEventListener('error', this.handleError);
    }

    setFatalHandler(handler: ((error: Error) => void) | null): void {
        this.fatalHandler = handler;
    }

    whenReady(): Promise<void> {
        return this.ready;
    }

    dispose(reason: Error = workerProtocolError('WorkerTerminatedError', 'worker transport was closed')): void {
        this.disposeInternal(reason, false);
    }

    open(request: WorkerOpenRequest): Promise<void> {
        return this.request('open', [request]);
    }

    close(): Promise<void> {
        return this.request('close', []);
    }

    destroy(): Promise<void> {
        return this.request('destroy', []);
    }

    deleteDB(dbName: string): Promise<void> {
        return this.request('deleteDB', [dbName]);
    }

    begin(mode: TxMode): Promise<number> {
        return this.request('begin', [mode]);
    }

    commit(txId: number): Promise<number> {
        return this.request('commit', [txId]);
    }

    rollback(txId: number): Promise<void> {
        return this.request('rollback', [txId]);
    }

    createStore(txId: number, name: string, options?: CreateStoreOptions): Promise<void> {
        return this.request('createStore', [txId, name, options]);
    }

    dropStore(txId: number, name: string): Promise<void> {
        return this.request('dropStore', [txId, name]);
    }

    clearStore(txId: number, name: string): Promise<void> {
        return this.request('clearStore', [txId, name]);
    }

    get(txId: number, store: string, key: Uint8Array): Promise<Uint8Array | null> {
        return this.request('get', [txId, store, key]);
    }

    getMany(txId: number, store: string, keys: Array<Uint8Array>): Promise<Array<Uint8Array | null>> {
        return this.request('getMany', [txId, store, keys]);
    }

    has(txId: number, store: string, key: Uint8Array): Promise<boolean> {
        return this.request('has', [txId, store, key]);
    }

    put(txId: number, store: string, key: Uint8Array, value: Uint8Array, options?: PutOptions): Promise<void> {
        return this.request('put', [txId, store, key, value, options]);
    }

    putMany(
        txId: number,
        store: string,
        entries: Array<[Uint8Array, Uint8Array]>,
        options?: PutOptions
    ): Promise<void> {
        return this.request('putMany', [txId, store, entries, options]);
    }

    delete(txId: number, store: string, key: Uint8Array): Promise<boolean> {
        return this.request('delete', [txId, store, key]);
    }

    deleteMany(txId: number, store: string, keys: Array<Uint8Array>): Promise<void> {
        return this.request('deleteMany', [txId, store, keys]);
    }

    applyBatch(txId: number, store: string, ops: Array<BatchOp>): Promise<void> {
        return this.request('applyBatch', [txId, store, ops]);
    }

    scan(txId: number, store: string, range: Range): Promise<ScanItem[]> {
        return this.request('scan', [txId, store, range]);
    }

    getByIndex(txId: number, store: string, indexName: string, key: Uint8Array): Promise<Uint8Array | null> {
        return this.request('getByIndex', [txId, store, indexName, key]);
    }

    scanByIndex(txId: number, store: string, indexName: string, range: Range): Promise<ScanItem[]> {
        return this.request('scanByIndex', [txId, store, indexName, range]);
    }

    getIndexes(): Promise<IndexDef[]> {
        return this.request('getIndexes', []);
    }

    reconcileIndexes(txId: number, indexes: IndexDef[]): Promise<void> {
        return this.request('reconcileIndexes', [txId, indexes]);
    }

    listStores(): Promise<string[]> {
        return this.request('listStores', []);
    }

    getVersion(): Promise<number> {
        return this.request('getVersion', []);
    }

    changesSince(txId: number, options: ChangeFeedOptions): Promise<ChangeFeed> {
        return this.request('changesSince', [txId, options]);
    }

    setSchemaVersion(txId: number, version: number): Promise<void> {
        return this.request('setSchemaVersion', [txId, version]);
    }

    exportSnapshot(options?: ExportSnapshotOptions): Promise<Uint8Array> {
        return this.request('exportSnapshot', [options]);
    }

    importSnapshot(data: Uint8Array): Promise<void> {
        return this.request('importSnapshot', [data]);
    }

    reset(): Promise<void> {
        return this.request('reset', []);
    }

    compact(): Promise<CompactionResult> {
        return this.request('compact', []);
    }

    rebuild(): Promise<CompactionResult> {
        return this.request('rebuild', []);
    }

    stats(): Promise<DbStats> {
        return this.request('stats', []);
    }

    storageInfo(): Promise<StorageInfo> {
        return this.request('storageInfo', []);
    }

    requestPersistence(): Promise<boolean> {
        return this.request('requestPersistence', []);
    }

    setFailpoint(failpoint: DebugFailpoint): Promise<void> {
        return this.request('setFailpoint', [failpoint]);
    }

    async request<M extends WorkerCommand>(command: M, args: WorkerCommandArgs<M>): Promise<WorkerCommandResult<M>> {
        this.ensureOpen();
        await this.ready;
        this.ensureOpen();
        const id = this.nextRequestId;
        this.nextRequestId += 1;
        const prepared = prepareWorkerCommandPayload(command, args);
        const message: WorkerProtocolRequestMessage = {
            type: WORKER_PROTOCOL_REQUEST,
            version: WORKER_PROTOCOL_VERSION,
            id,
            command,
            args: prepared.args
        } as WorkerProtocolRequestMessage;
        return await new Promise<WorkerCommandResult<M>>((resolve, reject) => {
            const timeout =
                this.requestTimeoutMs > 0
                    ? setTimeout(() => {
                          this.pending.delete(id);
                          reject(
                              workerProtocolError(
                                  'WorkerRequestTimeoutError',
                                  `worker command ${command} timed out after ${this.requestTimeoutMs}ms`
                              )
                          );
                      }, this.requestTimeoutMs)
                    : null;
            this.pending.set(id, {
                command,
                resolve: (value) => resolve(value as WorkerCommandResult<M>),
                reject,
                timeout
            });
            try {
                if (prepared.transfer.length > 0) {
                    this.worker.postMessage(message, prepared.transfer);
                } else {
                    this.worker.postMessage(message);
                }
            } catch (error) {
                this.rejectPending(id, error instanceof Error ? error : new Error(String(error)));
            }
        });
    }

    private ensureOpen(): void {
        if (this.closed) {
            throw this.closeReason ?? workerProtocolError('WorkerTerminatedError', 'worker transport is closed');
        }
    }

    private handleMessage = (event: MessageEvent<unknown>): void => {
        const data = event.data;
        if (isWorkerProtocolReadyMessage(data)) {
            this.readyResolved = true;
            this.readyResolve?.();
            this.readyResolve = null;
            this.readyReject = null;
            return;
        }
        if (!isWorkerProtocolResponseMessage(data)) {
            return;
        }
        const pending = this.pending.get(data.id);
        if (!pending) {
            return;
        }
        this.pending.delete(data.id);
        clearPendingTimeout(pending);
        if (data.ok) {
            pending.resolve(decodeWorkerResponsePayload(pending.command, data.result));
            return;
        }
        pending.reject(deserializeWorkerError(data.error));
    };

    private handleMessageError = (): void => {
        this.disposeInternal(
            workerProtocolError('WorkerMessageError', 'worker message could not be deserialized'),
            true
        );
    };

    private handleError = (event: ErrorEvent): void => {
        const error =
            event.error instanceof Error
                ? event.error
                : workerProtocolError('WorkerError', event.message || 'worker runtime failed');
        this.disposeInternal(error, true);
    };

    private rejectPending(id: number, error: Error): void {
        const pending = this.pending.get(id);
        if (!pending) {
            return;
        }
        this.pending.delete(id);
        clearPendingTimeout(pending);
        pending.reject(error);
    }

    private disposeInternal(reason: Error, notifyFatal: boolean): void {
        if (this.closed) {
            return;
        }
        this.closed = true;
        this.closeReason = reason;
        this.worker.removeEventListener('message', this.handleMessage);
        this.worker.removeEventListener('messageerror', this.handleMessageError);
        this.worker.removeEventListener('error', this.handleError);
        if (!this.readyResolved) {
            this.readyReject?.(reason);
        }
        this.readyResolve = null;
        this.readyReject = null;
        for (const pending of this.pending.values()) {
            clearPendingTimeout(pending);
            pending.reject(reason);
        }
        this.pending.clear();
        if (notifyFatal) {
            this.fatalHandler?.(reason);
        }
    }
}
