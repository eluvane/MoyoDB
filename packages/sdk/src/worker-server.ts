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

    enqueue: SendResponse = (response, transfer = []) => {
        if (this.closed) return;
        const captured = captureWorkerPayload(response, transfer);
        if (this.queued.length > 0 && this.queuedBytes + captured.byteLength > MAX_WORKER_BATCH_BYTES) {
            this.flush();
        }
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
        if (this.queued.length >= MAX_WORKER_BATCH_MESSAGES || this.queuedBytes >= MAX_WORKER_BATCH_BYTES) {
            this.flush();
        }
    };

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
                responses.enqueue(response, transfer);
                remaining -= 1;
                // Flush fully settled batches in this task. The fallback releases partial replies.
                if (remaining === 0) responses.flush();
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
