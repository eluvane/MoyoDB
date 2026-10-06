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
                resolve,
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
        if (!this.pending.has(message.id)) return;
        const byteLength = payloadBytes(message);
        if (this.queuedRequests.length > 0 && this.queuedBytes + byteLength > MAX_WORKER_BATCH_BYTES) {
            this.flushRequests();
        }
        // A full batch posts in this turn, so postMessage is the snapshot.
        const releaseNow =
            this.queuedRequests.length + 1 >= MAX_WORKER_BATCH_MESSAGES ||
            this.queuedBytes + byteLength >= MAX_WORKER_BATCH_BYTES;
        if (releaseNow) {
            this.queuedRequests.push({ message, transfer, byteLength });
            this.queuedBytes += byteLength;
            this.flushRequests();
            return;
        }
        const captured = snapshotQueuedPayload(message, transfer);
        if (!this.pending.has(message.id)) return;
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

interface ViewGeometry {
    offset: number;
    length: number;
    dataView: boolean;
}

const STANDARD_VIEW_CONSTRUCTORS = new Set<object>([
    DataView,
    Uint8Array,
    Uint8ClampedArray,
    Uint16Array,
    Uint32Array,
    Int8Array,
    Int16Array,
    Int32Array,
    Float32Array,
    Float64Array,
    BigInt64Array,
    BigUint64Array
]);

/**
 * Freeze a queued payload so the later postMessage is its only structured clone.
 * Listed buffers move now. Other ArrayBuffers are copied. SharedArrayBuffers stay shared.
 */
function snapshotQueuedPayload<T>(
    value: T,
    transfer: readonly Transferable[]
): { value: T; transfer: Transferable[]; byteLength: number } {
    const moving = new Set<ArrayBuffer>();
    for (const item of transfer) {
        if (!(item instanceof ArrayBuffer) || isResizableBuffer(item)) {
            return captureWorkerPayload(value, transfer as Transferable[]);
        }
        moving.add(item);
    }
    const geometries = new Map<ArrayBufferView, ViewGeometry>();
    if (!isPlainPayload(value, geometries, new WeakSet<object>())) {
        return captureWorkerPayload(value, transfer as Transferable[]);
    }
    const relocated = new Map<ArrayBuffer, ArrayBuffer>();
    for (const buffer of moving) {
        relocated.set(buffer, moveArrayBuffer(buffer));
    }
    const owned: ArrayBuffer[] = [];
    const seen = new WeakMap<object, unknown>();
    const cloneValue = (item: unknown): unknown => {
        if (item === null || typeof item !== 'object') {
            return item;
        }
        const existing = seen.get(item);
        if (existing !== undefined) {
            return existing;
        }
        if (typeof SharedArrayBuffer !== 'undefined' && item instanceof SharedArrayBuffer) {
            seen.set(item, item);
            return item;
        }
        if (item instanceof ArrayBuffer) {
            const next = relocated.get(item) ?? item.slice(0);
            seen.set(item, next);
            owned.push(next);
            return next;
        }
        if (ArrayBuffer.isView(item)) {
            const geometry = geometries.get(item);
            if (!geometry) {
                throw workerProtocolError('WorkerProtocolError', 'worker payload could not be queued');
            }
            const buffer = item.buffer;
            const nextBuffer =
                typeof SharedArrayBuffer !== 'undefined' && buffer instanceof SharedArrayBuffer
                    ? buffer
                    : (cloneValue(buffer) as ArrayBuffer);
            const out = retargetView(item, nextBuffer, geometry);
            seen.set(item, out);
            return out;
        }
        if (Array.isArray(item)) {
            const out = new Array<unknown>(item.length);
            seen.set(item, out);
            for (let index = 0; index < item.length; index += 1) {
                if (index in item) {
                    out[index] = cloneValue(item[index]);
                }
            }
            return out;
        }
        const out: Record<string, unknown> = Object.getPrototypeOf(item) === null ? Object.create(null) : {};
        seen.set(item, out);
        const record = item as Record<string, unknown>;
        for (const key in record) {
            if (Object.prototype.hasOwnProperty.call(record, key)) {
                out[key] = cloneValue(record[key]);
            }
        }
        return out;
    };
    const cloned = cloneValue(value) as T;
    return { value: cloned, transfer: owned, byteLength: payloadBytes(cloned) };
}

function payloadBytes(value: unknown): number {
    let byteLength = 0;
    const seen = new WeakSet<object>();
    const visit = (item: unknown): void => {
        if (!Number.isFinite(byteLength)) return;
        if (typeof item === 'string') {
            byteLength += 8 + item.length * 2;
            return;
        }
        if (item === null || typeof item !== 'object') {
            byteLength += 8;
            return;
        }
        if (seen.has(item)) {
            return;
        }
        seen.add(item);
        byteLength += 8;
        if (item instanceof ArrayBuffer) {
            byteLength += item.byteLength;
        } else if (typeof SharedArrayBuffer !== 'undefined' && item instanceof SharedArrayBuffer) {
            byteLength += item.byteLength;
        } else if (ArrayBuffer.isView(item)) {
            visit(item.buffer);
        } else if (Array.isArray(item)) {
            if (Object.getPrototypeOf(item) !== Array.prototype || hasExtraKeys(item)) {
                byteLength = Number.POSITIVE_INFINITY;
                return;
            }
            for (let index = 0; index < item.length; index += 1) {
                const descriptor = Object.getOwnPropertyDescriptor(item, String(index));
                if (descriptor && !('value' in descriptor)) {
                    byteLength = Number.POSITIVE_INFINITY;
                    return;
                }
                visit(descriptor?.value);
            }
        } else if (item instanceof Map) {
            for (const [key, child] of item) {
                visit(key);
                visit(child);
            }
        } else if (item instanceof Set) {
            for (const child of item) {
                visit(child);
            }
        } else {
            const record = item as Record<string, unknown>;
            for (const key in record) {
                if (Object.prototype.hasOwnProperty.call(record, key)) {
                    const descriptor = Object.getOwnPropertyDescriptor(record, key);
                    // Let postMessage evaluate accessors and preserve special data keys.
                    if (key === '__proto__' || !descriptor || !('value' in descriptor)) {
                        byteLength = Number.POSITIVE_INFINITY;
                        return;
                    }
                    byteLength += 8 + key.length * 2;
                    visit(descriptor.value);
                }
            }
        }
    };
    visit(value);
    return byteLength;
}

function isPlainPayload(
    value: unknown,
    geometries: Map<ArrayBufferView, ViewGeometry>,
    seen: WeakSet<object>
): boolean {
    if (typeof value === 'function' || typeof value === 'symbol') {
        return false;
    }
    if (value === null || typeof value !== 'object') {
        return true;
    }
    if (seen.has(value)) {
        return true;
    }
    seen.add(value);
    if (typeof SharedArrayBuffer !== 'undefined' && value instanceof SharedArrayBuffer) {
        return !isResizableBuffer(value);
    }
    if (value instanceof ArrayBuffer) {
        return !isResizableBuffer(value);
    }
    if (ArrayBuffer.isView(value)) {
        if (!isStandardView(value)) {
            return false;
        }
        const buffer = value.buffer;
        if (typeof SharedArrayBuffer !== 'undefined' && buffer instanceof SharedArrayBuffer) {
            if (isResizableBuffer(buffer)) {
                return false;
            }
            geometries.set(value, viewGeometry(value));
            return true;
        }
        if (!(buffer instanceof ArrayBuffer) || isResizableBuffer(buffer)) {
            return false;
        }
        geometries.set(value, viewGeometry(value));
        return true;
    }
    if (value instanceof Map || value instanceof Set || value instanceof Date || value instanceof RegExp) {
        return false;
    }
    if (Array.isArray(value)) {
        if (hasExtraKeys(value) || hasEnumerableSymbols(value)) {
            return false;
        }
        for (const child of value) {
            if (!isPlainPayload(child, geometries, seen)) {
                return false;
            }
        }
        return true;
    }
    const proto = Object.getPrototypeOf(value);
    if (proto !== Object.prototype && proto !== null) {
        return false;
    }
    if (hasEnumerableSymbols(value)) {
        return false;
    }
    const record = value as Record<string, unknown>;
    for (const key in record) {
        if (Object.prototype.hasOwnProperty.call(record, key) && !isPlainPayload(record[key], geometries, seen)) {
            return false;
        }
    }
    return true;
}

function hasExtraKeys(value: readonly unknown[]): boolean {
    const record: object = value;
    for (const key in record) {
        if (!Object.prototype.hasOwnProperty.call(value, key)) {
            continue;
        }
        const index = Number(key);
        if (!Number.isInteger(index) || index < 0 || index >= value.length || String(index) !== key) {
            return true;
        }
    }
    return false;
}

function hasEnumerableSymbols(value: object): boolean {
    for (const symbol of Object.getOwnPropertySymbols(value)) {
        if (Object.prototype.propertyIsEnumerable.call(value, symbol)) {
            return true;
        }
    }
    return false;
}

function isStandardView(view: ArrayBufferView): boolean {
    return STANDARD_VIEW_CONSTRUCTORS.has(view.constructor);
}

function viewGeometry(view: ArrayBufferView): ViewGeometry {
    if (view instanceof DataView) {
        return { offset: view.byteOffset, length: view.byteLength, dataView: true };
    }
    return { offset: view.byteOffset, length: (view as Uint8Array).length, dataView: false };
}

function retargetView(view: ArrayBufferView, buffer: ArrayBufferLike, geometry: ViewGeometry): ArrayBufferView {
    if (geometry.dataView) {
        return new DataView(buffer, geometry.offset, geometry.length);
    }
    const Typed = view.constructor as new (
        buffer: ArrayBufferLike,
        byteOffset: number,
        length: number
    ) => ArrayBufferView;
    return new Typed(buffer, geometry.offset, geometry.length);
}

function moveArrayBuffer(buffer: ArrayBuffer): ArrayBuffer {
    const movable = buffer as ArrayBuffer & { transfer?: () => ArrayBuffer };
    if (typeof movable.transfer === 'function') {
        return movable.transfer();
    }
    return structuredClone(buffer, { transfer: [buffer] });
}

function isResizableBuffer(buffer: ArrayBuffer | SharedArrayBuffer): boolean {
    return (buffer as { resizable?: boolean }).resizable === true;
}
