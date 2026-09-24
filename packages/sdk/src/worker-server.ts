import type { AutocommitCommand, WorkerApi } from './worker-api';
import {
    WORKER_PROTOCOL_READY,
    WORKER_PROTOCOL_RESPONSE,
    WORKER_PROTOCOL_VERSION,
    decodeWorkerCommandPayload,
    isAutocommitCommand,
    packedBatchOpsBytes,
    packedBinaryListBytes,
    prepareWorkerResponsePayload,
    isWorkerCommand,
    isWorkerProtocolEnvelope,
    isWorkerProtocolRequestMessage,
    serializeWorkerError,
    workerProtocolError,
    type WorkerProtocolErrorMessage,
    type WorkerProtocolResponseMessage,
    type WorkerProtocolSuccessMessage,
    type WorkerCommand
} from './worker-protocol';

type PackedWorkerApi = WorkerApi & {
    getManyPacked?: (txId: number, store: string, packedKeys: Uint8Array) => Promise<unknown>;
    putManyPacked?: (txId: number, store: string, packedEntries: Uint8Array, options?: unknown) => Promise<unknown>;
    deleteManyPacked?: (txId: number, store: string, packedKeys: Uint8Array) => Promise<unknown>;
    applyBatchPacked?: (txId: number, store: string, packedOps: Uint8Array) => Promise<unknown>;
};

export interface WorkerServerHandle {
    close(): void;
}

/** Commands whose first argument is a transaction handle; they run in that transaction's lane. */
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
    'reconcileIndexes',
    'setSchemaVersion'
]);

/** Commands that replace or tear down engine state and must not overlap any other engine work. */
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
 * Commands that never touch the open engine. They bypass scheduling so a slow
 * browser API (storage estimate, persistence prompt) cannot stall a close.
 */
const UNSCHEDULED_COMMANDS = new Set<WorkerCommand>(['deleteDB', 'storageInfo', 'requestPersistence']);

const noop = () => {};

/**
 * Orders requests the way the engine needs them:
 * - operations of one transaction run strictly one after another, so an
 *   awaited step (compression, uniqueness check) cannot interleave with the
 *   next operation of the same transaction;
 * - readwrite autocommits queue behind each other instead of failing on the
 *   single-writer rule;
 * - exclusive commands wait for all in-flight work and hold back new work
 *   until they finish.
 */
class RequestScheduler {
    #lanes = new Map<number | string, Promise<void>>();
    #inFlight = new Set<Promise<void>>();
    #barrier: Promise<void> = Promise.resolve();

    schedule<T>(command: WorkerCommand, args: unknown[], task: () => Promise<T>): Promise<T> {
        if (UNSCHEDULED_COMMANDS.has(command)) {
            return task();
        }
        if (EXCLUSIVE_COMMANDS.has(command)) {
            return this.#runExclusive(task);
        }
        return this.#runShared(laneFor(command, args), task);
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
        void settled.then(() => {
            this.#inFlight.delete(settled);
        });
        if (lane !== null) {
            this.#lanes.set(lane, settled);
            void settled.then(() => {
                if (this.#lanes.get(lane) === settled) {
                    this.#lanes.delete(lane);
                }
            });
        }
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

export function exposeWorkerApi(
    api: WorkerApi,
    scope: DedicatedWorkerGlobalScope = self as DedicatedWorkerGlobalScope
): WorkerServerHandle {
    const scheduler = new RequestScheduler();
    const handleMessage = (event: MessageEvent<unknown>) => {
        void dispatchWorkerRequest(scope, api, scheduler, event.data);
    };
    scope.addEventListener('message', handleMessage);
    scope.postMessage({
        type: WORKER_PROTOCOL_READY,
        version: WORKER_PROTOCOL_VERSION
    });
    return {
        close() {
            scope.removeEventListener('message', handleMessage);
        }
    };
}

async function dispatchWorkerRequest(
    scope: DedicatedWorkerGlobalScope,
    api: WorkerApi,
    scheduler: RequestScheduler,
    data: unknown
): Promise<void> {
    if (!isWorkerProtocolEnvelope(data)) {
        return;
    }
    const id = typeof data.id === 'number' && Number.isSafeInteger(data.id) ? data.id : 0;
    if (!isWorkerProtocolRequestMessage(data)) {
        postWorkerResponse(
            scope,
            errorResponse(id, workerProtocolError('WorkerProtocolError', 'invalid worker protocol request'))
        );
        return;
    }
    if (!isWorkerCommand(data.command)) {
        postWorkerResponse(
            scope,
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
        postWorkerResponse(scope, successResponse(data.id, responsePayload.result), responsePayload.transfer);
    } catch (error) {
        postWorkerResponse(scope, errorResponse(data.id, error));
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
    const method = api[command] as unknown as (...methodArgs: unknown[]) => Promise<unknown>;
    return method.apply(api, decodeWorkerCommandPayload(command, args as never) as unknown[]);
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
    } as WorkerProtocolSuccessMessage;
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
    scope: DedicatedWorkerGlobalScope,
    response: WorkerProtocolResponseMessage,
    transfer: Transferable[] = []
): void {
    if (response.ok && transfer.length > 0) {
        try {
            scope.postMessage(response, transfer);
            return;
        } catch {}
    }
    scope.postMessage(response);
}
