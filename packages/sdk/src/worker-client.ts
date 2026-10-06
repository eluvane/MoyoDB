import type {
    AutocommitCommand,
    IndexScanPage,
    WorkerApi,
    WorkerOpenRequest,
    WorkerScanPage,
    WorkerScanPageRequest
} from './worker-api';
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
    MAX_WORKER_BATCH_BYTES,
    MAX_WORKER_BATCH_MESSAGES,
    WORKER_PROTOCOL_REQUEST,
    WORKER_PROTOCOL_REQUEST_BATCH,
    WORKER_PROTOCOL_RESPONSE,
    WORKER_PROTOCOL_RESPONSE_BATCH,
    WORKER_PROTOCOL_VERSION,
    captureWorkerPayload,
    deserializeWorkerError,
    decodeWorkerResponsePayload,
    isWorkerProtocolReadyMessage,
    isWorkerProtocolReadyEnvelope,
    isWorkerProtocolResponseBatchMessage,
    isWorkerProtocolResponseMessage,
    prepareWorkerCommandPayload,
    workerProtocolError,
    type AutocommitArgs,
    type WorkerCommand,
    type WorkerCommandArgs,
    type WorkerCommandResult,
    type WorkerProtocolRequestBatchMessage,
    type WorkerProtocolRequestMessage
} from './worker-protocol';
import { clampTimerDelayMs, isRecord } from './internal';

interface PendingRequest {
    /** Response command; autocommit replies use the inner command's format. */
    command: WorkerCommand;
    resolve: (value: unknown) => void;
    reject: (error: Error) => void;
    timeout: ReturnType<typeof setTimeout> | null;
}

interface QueuedRequest {
    message: WorkerProtocolRequestMessage;
    transfer: Transferable[];
    byteLength: number;
}

function clearPendingTimeout(pending: PendingRequest): void {
    if (pending.timeout) {
        clearTimeout(pending.timeout);
        pending.timeout = null;
    }
}

export interface WorkerProtocolClientOptions {
    requestTimeoutMs?: number;
    readyTimeoutMs?: number;
    /** Keeps scan packets packed when relaying through SharedWorker. */
    forwardScanPackets?: boolean;
}

export interface WorkerTransport {
    postMessage(message: unknown, transfer?: Transferable[]): void;
    addEventListener(type: string, listener: EventListenerOrEventListenerObject): void;
    removeEventListener(type: string, listener: EventListenerOrEventListenerObject): void;
}

export interface WorkerClient extends WorkerApi {
    autocommit<M extends AutocommitCommand>(
        mode: TxMode,
        command: M,
        args: AutocommitArgs<M>
    ): Promise<WorkerCommandResult<M>>;
    request<M extends WorkerCommand>(command: M, args: WorkerCommandArgs<M>): Promise<WorkerCommandResult<M>>;
    setFatalHandler(handler: ((error: Error) => void) | null): void;
    whenReady(): Promise<void>;
    dispose(reason?: Error): void;
    acquireMigration?(): Promise<void>;
    releaseMigration?(): Promise<void>;
    setTransactionsInvalidatedHandler?(handler: (() => void) | null): void;
    crash?(): void;
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
    private batching = false;
    private queuedRequests: QueuedRequest[] = [];
    private queuedBytes = 0;
    private flushScheduled = false;
    private awaitingReady = 0;
    private readyTimeout: ReturnType<typeof setTimeout> | null = null;
    private forwardScanPackets: boolean;

    constructor(
        private readonly worker: WorkerTransport,
        options: WorkerProtocolClientOptions = {}
    ) {
        this.forwardScanPackets = options.forwardScanPackets === true;
        this.requestTimeoutMs = clampTimerDelayMs(options.requestTimeoutMs ?? 0);
        this.ready = new Promise<void>((resolve, reject) => {
            this.readyResolve = resolve;
            this.readyReject = reject;
        });
        const readyTimeoutMs = clampTimerDelayMs(options.readyTimeoutMs ?? 0);
        if (readyTimeoutMs > 0) {
            this.readyTimeout = setTimeout(() => {
                this.fail(workerProtocolError('WorkerReadyTimeoutError', 'worker did not become ready'));
            }, readyTimeoutMs);
        }
        void this.ready.catch(() => undefined);
        this.worker.addEventListener('message', this.handleMessage as EventListener);
        this.worker.addEventListener('messageerror', this.handleMessageError);
        this.worker.addEventListener('error', this.handleError as EventListener);
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

    protected fail(reason: Error): void {
        this.disposeInternal(reason, true);
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

    autocommit<M extends AutocommitCommand>(
        mode: TxMode,
        command: M,
        args: AutocommitArgs<M>
    ): Promise<WorkerCommandResult<M>> {
        return this.send('autocommit', [mode, command, args as unknown[]], command) as Promise<WorkerCommandResult<M>>;
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

    scanPage(request: WorkerScanPageRequest): Promise<WorkerScanPage> {
        return this.request('scanPage', [request]);
    }

    closeCursor(cursorId: number): Promise<void> {
        return this.request('closeCursor', [cursorId]);
    }

    getByIndex(txId: number, store: string, indexName: string, key: Uint8Array): Promise<Uint8Array | null> {
        return this.request('getByIndex', [txId, store, indexName, key]);
    }

    scanByIndex(txId: number, store: string, indexName: string, range: Range): Promise<ScanItem[]> {
        return this.request('scanByIndex', [txId, store, indexName, range]);
    }

    scanByIndexPage(
        txId: number,
        store: string,
        indexName: string,
        range: Range,
        cursor: Uint8Array | null,
        limit: number,
        maxBytes?: number
    ): Promise<IndexScanPage> {
        return this.request('scanByIndexPage', [txId, store, indexName, range, cursor, limit, maxBytes]);
    }

    getIndexes(txId?: number): Promise<IndexDef[]> {
        return this.request('getIndexes', txId === undefined ? [] : [txId]);
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
        return (await this.send(command, args, command)) as WorkerCommandResult<M>;
    }

    private async send<M extends WorkerCommand>(
        command: M,
        args: WorkerCommandArgs<M>,
        responseCommand: WorkerCommand
    ): Promise<unknown> {
        this.ensureOpen();
        this.awaitingReady += 1;
        try {
            await this.ready;
        } finally {
            this.awaitingReady -= 1;
        }
        this.ensureOpen();
        // Calls awaiting readiness together can share a batch. A lone call posts immediately.
        const shouldQueue = this.batching && (this.awaitingReady > 0 || this.queuedRequests.length > 0);
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
        return await new Promise<unknown>((resolve, reject) => {
            const timeout =
                this.requestTimeoutMs > 0
                    ? setTimeout(() => {
                          const current = this.pending.get(id);
                          if (!current || current.timeout === null) {
                              return;
                          }
                          this.pending.delete(id);
                          current.timeout = null;
                          reject(
                              workerProtocolError(
                                  'WorkerRequestTimeoutError',
                                  `worker command ${command} timed out after ${this.requestTimeoutMs}ms`
                              )
                          );
                      }, this.requestTimeoutMs)
                    : null;
            this.pending.set(id, {
                command: responseCommand,
                resolve: (value) => resolve(value),
                reject,
                timeout
            });
            try {
                if (shouldQueue) {
                    this.queueRequest(message, prepared.transfer);
                } else if (prepared.transfer.length > 0) {
                    this.worker.postMessage(message, prepared.transfer);
                } else {
                    this.worker.postMessage(message);
                }
            } catch (error) {
                this.rejectPending(id, error instanceof Error ? error : new Error(String(error)));
            }
        });
    }

    private queueRequest(message: WorkerProtocolRequestMessage, transfer: Transferable[]): void {
        const captured = captureWorkerPayload(message, transfer);
        if (!this.pending.has(message.id)) return;
        if (this.queuedRequests.length > 0 && this.queuedBytes + captured.byteLength > MAX_WORKER_BATCH_BYTES) {
            this.flushRequests();
        }
        this.queuedRequests.push({
            message: captured.value,
            transfer: captured.transfer,
            byteLength: captured.byteLength
        });
        this.queuedBytes += captured.byteLength;
        if (!this.flushScheduled) {
            this.flushScheduled = true;
            queueMicrotask(() => {
                this.flushScheduled = false;
                this.flushRequests();
            });
        }
        if (this.queuedRequests.length >= MAX_WORKER_BATCH_MESSAGES || this.queuedBytes >= MAX_WORKER_BATCH_BYTES) {
            this.flushRequests();
        }
    }

    private flushRequests(): void {
        const queued = this.queuedRequests;
        this.queuedRequests = [];
        this.queuedBytes = 0;
        if (this.closed) return;
        const active = queued.filter((entry) => this.pending.has(entry.message.id));
        if (active.length === 0) return;
        const message: WorkerProtocolRequestMessage | WorkerProtocolRequestBatchMessage =
            active.length === 1
                ? active[0].message
                : {
                      type: WORKER_PROTOCOL_REQUEST_BATCH,
                      version: WORKER_PROTOCOL_VERSION,
                      requests: active.map((entry) => entry.message)
                  };
        const transfer: Transferable[] = [];
        for (const entry of active) {
            for (const buffer of entry.transfer) transfer.push(buffer);
        }
        try {
            if (transfer.length > 0) {
                this.worker.postMessage(message, transfer);
            } else {
                this.worker.postMessage(message);
            }
        } catch (error) {
            const reason = error instanceof Error ? error : new Error(String(error));
            for (const entry of active) this.rejectPending(entry.message.id, reason);
        }
    }

    private ensureOpen(): void {
        if (this.closed) {
            throw this.closeReason ?? workerProtocolError('WorkerTerminatedError', 'worker transport is closed');
        }
    }

    private handleMessage = (event: MessageEvent<unknown>): void => {
        const data = event.data;
        if (isWorkerProtocolReadyMessage(data)) {
            if (this.readyResolved) return;
            this.batching = data.batching === 1;
            this.readyResolved = true;
            if (this.readyTimeout) clearTimeout(this.readyTimeout);
            this.readyTimeout = null;
            this.readyResolve?.();
            this.readyResolve = null;
            this.readyReject = null;
            return;
        }
        if (isWorkerProtocolReadyEnvelope(data)) {
            this.fail(workerProtocolError('WorkerProtocolError', 'incompatible worker protocol ready message'));
            return;
        }
        if (
            isRecord(data) &&
            (data.type === WORKER_PROTOCOL_RESPONSE || data.type === WORKER_PROTOCOL_RESPONSE_BATCH) &&
            data.version !== WORKER_PROTOCOL_VERSION
        ) {
            this.fail(workerProtocolError('WorkerProtocolError', 'incompatible worker protocol response'));
            return;
        }
        if (isWorkerProtocolResponseBatchMessage(data)) {
            for (const response of data.responses) this.handleResponse(response);
            return;
        }
        if (
            isRecord(data) &&
            data.type === WORKER_PROTOCOL_RESPONSE_BATCH &&
            data.version === WORKER_PROTOCOL_VERSION
        ) {
            this.fail(workerProtocolError('WorkerProtocolError', 'invalid worker response batch'));
            return;
        }
        this.handleResponse(data);
    };

    private handleResponse(data: unknown): void {
        if (!isWorkerProtocolResponseMessage(data)) {
            if (
                isRecord(data) &&
                data.type === WORKER_PROTOCOL_RESPONSE &&
                data.version === WORKER_PROTOCOL_VERSION &&
                typeof data.id === 'number' &&
                Number.isSafeInteger(data.id)
            ) {
                this.rejectPending(data.id, workerProtocolError('WorkerProtocolError', 'invalid worker response'));
            }
            return;
        }
        const pending = this.pending.get(data.id);
        if (!pending) {
            return;
        }
        this.pending.delete(data.id);
        clearPendingTimeout(pending);
        try {
            if (data.ok) {
                pending.resolve(
                    this.forwardScanPackets &&
                        (pending.command === 'scanPage' ||
                            pending.command === 'scan' ||
                            pending.command === 'scanByIndex' ||
                            pending.command === 'scanByIndexPage')
                        ? data.result
                        : decodeWorkerResponsePayload(pending.command, data.result)
                );
            } else {
                pending.reject(deserializeWorkerError(data.error));
            }
        } catch (error) {
            pending.reject(
                error instanceof Error && error.name === 'WorkerProtocolError'
                    ? error
                    : workerProtocolError('WorkerProtocolError', 'worker response could not be decoded')
            );
        }
    }

    private handleMessageError = (): void => {
        this.fail(workerProtocolError('WorkerMessageError', 'worker message could not be deserialized'));
    };

    private handleError = (event: ErrorEvent): void => {
        const error =
            event.error instanceof Error
                ? event.error
                : workerProtocolError('WorkerError', event.message || 'worker runtime failed');
        this.fail(error);
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
        this.worker.removeEventListener('message', this.handleMessage as EventListener);
        this.worker.removeEventListener('messageerror', this.handleMessageError);
        this.worker.removeEventListener('error', this.handleError as EventListener);
        if (this.readyTimeout) clearTimeout(this.readyTimeout);
        this.readyTimeout = null;
        if (!this.readyResolved) {
            this.readyReject?.(reason);
        }
        this.readyResolve = null;
        this.readyReject = null;
        this.queuedRequests = [];
        this.queuedBytes = 0;
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
