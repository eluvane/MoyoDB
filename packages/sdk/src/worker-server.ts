import type { AutocommitCommand, WorkerApi } from './worker-api';
import {
    MAX_WORKER_BATCH_BYTES,
    MAX_WORKER_BATCH_MESSAGES,
    WORKER_PROTOCOL_READY,
    WORKER_PROTOCOL_RESPONSE,
    WORKER_PROTOCOL_RESPONSE_BATCH,
    WORKER_PROTOCOL_VERSION,
    captureWorkerPayload,
    decodeWorkerCommandPayload,
    isAutocommitCommand,
    packedBatchOpsBytes,
    packedBinaryListBytes,
    prepareWorkerResponsePayload,
    isWorkerCommand,
    isWorkerProtocolEnvelope,
    isWorkerProtocolRequestBatchMessage,
    isWorkerProtocolRequestMessage,
    serializeWorkerError,
    workerProtocolError,
    type WorkerProtocolErrorMessage,
    type WorkerProtocolResponseMessage,
    type WorkerProtocolResponseBatchMessage,
    type WorkerProtocolSuccessMessage,
    type WorkerCommand
} from './worker-protocol';
import { isRecord } from './internal';

type PackedWorkerApi = WorkerApi & {
    getManyPacked?: (txId: number, store: string, packedKeys: Uint8Array) => Promise<unknown>;
    putManyPacked?: (txId: number, store: string, packedEntries: Uint8Array, options?: unknown) => Promise<unknown>;
    deleteManyPacked?: (txId: number, store: string, packedKeys: Uint8Array) => Promise<unknown>;
    applyBatchPacked?: (txId: number, store: string, packedOps: Uint8Array) => Promise<unknown>;
};

export interface WorkerServerHandle {
    close(): void;
}

export interface WorkerServerScope {
    postMessage(message: unknown, transfer?: Transferable[]): void;
    addEventListener(type: 'message', listener: (event: MessageEvent<unknown>) => void): void;
    removeEventListener(type: 'message', listener: (event: MessageEvent<unknown>) => void): void;
    readonly location: { readonly origin: string };
}

/** The first argument identifies the transaction lane. */
const TRANSACTION_COMMANDS = new Set<WorkerCommand>([
    'commit',
    'rollback',
    'createStore',
    'dropStore',
    'clearStore',
    'get',
    'getMany',
    'has',
    'put',
    'putMany',
    'delete',
    'deleteMany',
    'applyBatch',
    'scan',
    'getByIndex',
    'scanByIndex',
    'scanByIndexPage',
    'getIndexes',
    'reconcileIndexes',
    'setSchemaVersion'
]);

/** These commands require exclusive access to the open engine. */
const EXCLUSIVE_COMMANDS = new Set<WorkerCommand>([
    'open',
    'close',
    'destroy',
    'importSnapshot',
    'reset',
    'compact',
    'rebuild'
]);

/**
 * These commands do not use the open engine, so browser storage calls
 * can run without delaying its close.
 */
const UNSCHEDULED_COMMANDS = new Set<WorkerCommand>(['deleteDB', 'storageInfo', 'requestPersistence']);

const noop = () => {};

/**
 * Serializes each transaction lane, including awaited work. Readwrite
 * autocommits share one writer lane. Exclusive commands wait for scheduled
 * work and block later scheduled work until they finish.
 */
class RequestScheduler {
    #lanes = new Map<number | string, Promise<void>>();
    #inFlight = new Set<Promise<void>>();
    #barrier: Promise<void> = Promise.resolve();
    #cursorLanes = new Map<number, number | string>();

    schedule<T>(command: WorkerCommand, args: unknown[], task: () => Promise<T>): Promise<T> {
        if (UNSCHEDULED_COMMANDS.has(command)) {
            return task();
        }
        if (EXCLUSIVE_COMMANDS.has(command)) {
            if (this.#cursorLanes.size === 0) return this.#runExclusive(task);
            return this.#runExclusive(async () => {
                const result = await task();
                this.#cursorLanes.clear();
                return result;
            });
        }
        if (
            command !== 'scanPage' &&
            command !== 'closeCursor' &&
            !((command === 'commit' || command === 'rollback') && this.#cursorLanes.size > 0)
        ) {
            return this.#runShared(laneFor(command, args), task);
        }
        const request = command === 'scanPage' && isRecord(args[0]) ? args[0] : null;
        const cursorId = request?.cursorId ?? (command === 'closeCursor' ? args[0] : undefined);
        const cursorLane = typeof cursorId === 'number' ? this.#cursorLanes.get(cursorId) : undefined;
        // Explicit cursors share their transaction lane. Implicit cursors own a separate read snapshot.
        const lane =
            cursorLane ??
            (request && typeof request.txId === 'number'
                ? request.txId
                : typeof cursorId === 'number'
                  ? `cursor:${cursorId}`
                  : laneFor(command, args));
        return this.#runShared(lane, async () => {
            try {
                const result = await task();
                if (request && isRecord(result)) {
                    if (typeof cursorId === 'number') this.#cursorLanes.delete(cursorId);
                    if (typeof result.cursorId === 'number') {
                        this.#cursorLanes.set(result.cursorId, lane ?? `cursor:${result.cursorId}`);
                    }
                }
                return result;
            } finally {
                if (command === 'closeCursor' && typeof cursorId === 'number') this.#cursorLanes.delete(cursorId);
                if (command === 'commit' || command === 'rollback') {
                    for (const [cursor, ownerLane] of this.#cursorLanes) {
                        if (ownerLane === args[0]) this.#cursorLanes.delete(cursor);
                    }
                }
            }
        });
    }

    #runShared<T>(lane: number | string | null, task: () => Promise<T>): Promise<T> {
        const barrier = this.#barrier;
        const previous = lane === null ? undefined : this.#lanes.get(lane);
        const result = (async () => {
            await barrier;
            if (previous) {
                await previous;
            }
            return task();
        })();
        const settled = result.then(noop, noop);
        this.#inFlight.add(settled);
        if (lane !== null) {
            this.#lanes.set(lane, settled);
        }
        void settled.then(() => {
            this.#inFlight.delete(settled);
            if (lane !== null && this.#lanes.get(lane) === settled) {
                this.#lanes.delete(lane);
            }
        });
        return result;
    }

    #runExclusive<T>(task: () => Promise<T>): Promise<T> {
        const previousBarrier = this.#barrier;
        const pending = Array.from(this.#inFlight);
        const result = (async () => {
            await previousBarrier;
            await Promise.all(pending);
            return task();
        })();
        this.#barrier = result.then(noop, noop);
        return result;
    }
}

const AUTOCOMMIT_WRITER_LANE = 'autocommit:readwrite';

function laneFor(command: WorkerCommand, args: unknown[]): number | string | null {
    if (TRANSACTION_COMMANDS.has(command) && typeof args[0] === 'number') {
        return args[0];
    }
    if (command === 'autocommit' && args[0] === 'readwrite') {
        return AUTOCOMMIT_WRITER_LANE;
    }
    return null;
}

type SendResponse = (response: WorkerProtocolResponseMessage, transfer?: Transferable[]) => void;

class ResponseQueue {
    private queued: Array<{ response: WorkerProtocolResponseMessage; transfer: Transferable[] }> = [];
    private queuedBytes = 0;
    private flushScheduled = false;
    private flushChannel: MessageChannel | null = null;
    private closed = false;

    constructor(private readonly scope: WorkerServerScope) {}

    enqueue(response: WorkerProtocolResponseMessage, transfer: Transferable[] = [], flushNow = false): void {
        if (this.closed) return;
        const byteLength = payloadBytes(response);
        if (this.queued.length > 0 && this.queuedBytes + byteLength > MAX_WORKER_BATCH_BYTES) {
            this.flush();
        }
        // Posting before the next turn is the snapshot. Deferred replies are frozen first.
        const releaseNow =
            flushNow ||
            this.queued.length + 1 >= MAX_WORKER_BATCH_MESSAGES ||
            this.queuedBytes + byteLength >= MAX_WORKER_BATCH_BYTES;
        if (releaseNow) {
            this.queued.push({ response, transfer });
            this.queuedBytes += byteLength;
            this.flush();
            return;
        }
        const captured = snapshotQueuedPayload(response, transfer);
        this.queued.push({ response: captured.value, transfer: captured.transfer });
        this.queuedBytes += captured.byteLength;
        if (!this.flushScheduled) {
            this.flushScheduled = true;
            queueMicrotask(() => {
                if (this.closed || this.queued.length === 0) {
                    this.flushScheduled = false;
                    return;
                }
                // A task boundary groups replies from successive transaction-lane
                // microtasks and releases ready replies while other requests remain pending.
                this.flushChannel ??= this.createFlushChannel();
                this.flushChannel.port2.postMessage(null);
            });
        }
    }

    close(): void {
        this.closed = true;
        this.queued = [];
        this.queuedBytes = 0;
        this.flushChannel?.port1.close();
        this.flushChannel?.port2.close();
        this.flushChannel = null;
    }

    private createFlushChannel(): MessageChannel {
        const channel = new MessageChannel();
        channel.port1.addEventListener('message', () => {
            this.flushScheduled = false;
            this.flush();
        });
        channel.port1.start();
        return channel;
    }

    flush(): void {
        const queued = this.queued;
        this.queued = [];
        this.queuedBytes = 0;
        if (this.closed || queued.length === 0) return;
        const response: WorkerProtocolResponseMessage | WorkerProtocolResponseBatchMessage =
            queued.length === 1
                ? queued[0].response
                : {
                      type: WORKER_PROTOCOL_RESPONSE_BATCH,
                      version: WORKER_PROTOCOL_VERSION,
                      responses: queued.map((entry) => entry.response)
                  };
        const transfer: Transferable[] = [];
        for (const entry of queued) {
            for (const buffer of entry.transfer) transfer.push(buffer);
        }
        postWorkerResponse(this.scope, response, transfer);
    }
}

export function exposeWorkerApi(api: WorkerApi, scope: WorkerServerScope = self): WorkerServerHandle {
    const scheduler = new RequestScheduler();
    const responses = new ResponseQueue(scope);
    const handleMessage = (event: MessageEvent<unknown>) => {
        if (event.origin !== '' && event.origin !== scope.location.origin) {
            return;
        }
        if (isWorkerProtocolRequestBatchMessage(event.data)) {
            let remaining = event.data.requests.length;
            const respond: SendResponse = (response, transfer) => {
                remaining -= 1;
                // Flush fully settled batches in this task. The fallback releases partial replies.
                responses.enqueue(response, transfer, remaining === 0);
            };
            for (const request of event.data.requests) {
                void dispatchWorkerRequest(scope, api, scheduler, request, respond);
            }
            return;
        }
        void dispatchWorkerRequest(scope, api, scheduler, event.data);
    };
    scope.addEventListener('message', handleMessage);
    scope.postMessage({
        type: WORKER_PROTOCOL_READY,
        version: WORKER_PROTOCOL_VERSION,
        batching: 1
    });
    return {
        close() {
            scope.removeEventListener('message', handleMessage);
            responses.close();
        }
    };
}

async function dispatchWorkerRequest(
    scope: WorkerServerScope,
    api: WorkerApi,
    scheduler: RequestScheduler,
    data: unknown,
    respond: SendResponse = (response, transfer) => postWorkerResponse(scope, response, transfer)
): Promise<void> {
    if (!isWorkerProtocolEnvelope(data)) {
        return;
    }
    const id = typeof data.id === 'number' && Number.isSafeInteger(data.id) ? data.id : 0;
    if (!isWorkerProtocolRequestMessage(data)) {
        respond(errorResponse(id, workerProtocolError('WorkerProtocolError', 'invalid worker protocol request')));
        return;
    }
    if (!isWorkerCommand(data.command)) {
        respond(
            errorResponse(
                data.id,
                workerProtocolError('WorkerProtocolError', `unsupported worker command: ${String(data.command)}`)
            )
        );
        return;
    }
    const command = data.command;
    const args = data.args as unknown[];
    try {
        let responseCommand: WorkerCommand = command;
        let invoke: () => Promise<unknown>;
        if (command === 'autocommit') {
            const [mode, inner, innerArgs] = args;
            if (
                (mode !== 'readonly' && mode !== 'readwrite') ||
                !isAutocommitCommand(inner) ||
                !Array.isArray(innerArgs)
            ) {
                throw workerProtocolError('WorkerProtocolError', 'invalid autocommit request');
            }
            assertImplemented(api, inner);
            responseCommand = inner;
            invoke = () => runAutocommit(api, mode, inner, innerArgs);
        } else {
            assertImplemented(api, command);
            invoke = () => invokeCommand(api, command, args);
        }
        const result = await scheduler.schedule(command, args, invoke);
        const responsePayload = prepareWorkerResponsePayload(responseCommand, result as never);
        respond(successResponse(data.id, responsePayload.result), responsePayload.transfer);
    } catch (error) {
        respond(errorResponse(data.id, error));
    }
}

function assertImplemented(api: WorkerApi, command: WorkerCommand): void {
    if (typeof (api[command] as unknown) !== 'function') {
        throw workerProtocolError('WorkerProtocolError', `worker command is not implemented: ${command}`);
    }
}

function invokeCommand(api: WorkerApi, command: WorkerCommand, args: unknown[]): Promise<unknown> {
    const packedResult = dispatchPackedCommand(api, command, args);
    if (packedResult !== null) {
        return packedResult;
    }
    return (api[command] as unknown as (...methodArgs: unknown[]) => Promise<unknown>).apply(
        api,
        decodeWorkerCommandPayload(command, args as never) as unknown[]
    );
}

async function runAutocommit(
    api: WorkerApi,
    mode: 'readonly' | 'readwrite',
    command: AutocommitCommand,
    args: unknown[]
): Promise<unknown> {
    const txId = await api.begin(mode);
    let result: unknown;
    try {
        result = await invokeCommand(api, command, [txId, ...args]);
    } catch (error) {
        try {
            await api.rollback(txId);
        } catch {}
        throw error;
    }
    if (mode === 'readwrite') {
        await api.commit(txId);
    } else {
        await api.rollback(txId);
    }
    return result;
}

function dispatchPackedCommand(api: WorkerApi, command: WorkerCommand, args: unknown[]): Promise<unknown> | null {
    const packedApi = api as PackedWorkerApi;
    if (command === 'getMany' || command === 'deleteMany') {
        const [txId, store, keys] = args;
        if (typeof txId !== 'number' || typeof store !== 'string') {
            return null;
        }
        const packedKeys = packedBinaryListBytes(keys);
        if (packedKeys === null) {
            return null;
        }
        if (command === 'getMany' && typeof packedApi.getManyPacked === 'function') {
            return packedApi.getManyPacked(txId, store, packedKeys);
        }
        if (command === 'deleteMany' && typeof packedApi.deleteManyPacked === 'function') {
            return packedApi.deleteManyPacked(txId, store, packedKeys);
        }
        return null;
    }

    if (command === 'putMany') {
        const [txId, store, entries, options] = args;
        if (typeof txId !== 'number' || typeof store !== 'string' || typeof packedApi.putManyPacked !== 'function') {
            return null;
        }
        const packedEntries = packedBinaryListBytes(entries);
        return packedEntries === null ? null : packedApi.putManyPacked(txId, store, packedEntries, options);
    }

    if (command === 'applyBatch') {
        const [txId, store, ops] = args;
        if (typeof txId !== 'number' || typeof store !== 'string' || typeof packedApi.applyBatchPacked !== 'function') {
            return null;
        }
        const packedOps = packedBatchOpsBytes(ops);
        return packedOps === null ? null : packedApi.applyBatchPacked(txId, store, packedOps);
    }

    return null;
}

function successResponse(id: number, result: unknown): WorkerProtocolSuccessMessage {
    return {
        type: WORKER_PROTOCOL_RESPONSE,
        version: WORKER_PROTOCOL_VERSION,
        id,
        ok: true,
        result
    };
}

function errorResponse(id: number, error: unknown): WorkerProtocolErrorMessage {
    return {
        type: WORKER_PROTOCOL_RESPONSE,
        version: WORKER_PROTOCOL_VERSION,
        id,
        ok: false,
        error: serializeWorkerError(error)
    };
}

function postWorkerResponse(
    scope: WorkerServerScope,
    response: WorkerProtocolResponseMessage | WorkerProtocolResponseBatchMessage,
    transfer: Transferable[] = []
): void {
    if (transfer.length > 0) {
        try {
            scope.postMessage(response, transfer);
            return;
        } catch {}
    }
    scope.postMessage(response);
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
