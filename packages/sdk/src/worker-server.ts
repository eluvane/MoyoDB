import type { WorkerApi } from './worker-api';
import {
    WORKER_PROTOCOL_READY,
    WORKER_PROTOCOL_RESPONSE,
    WORKER_PROTOCOL_VERSION,
    decodeWorkerCommandPayload,
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

export function exposeWorkerApi(
    api: WorkerApi,
    scope: DedicatedWorkerGlobalScope = self as DedicatedWorkerGlobalScope
): WorkerServerHandle {
    const handleMessage = (event: MessageEvent<unknown>) => {
        void dispatchWorkerRequest(scope, api, event.data);
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

async function dispatchWorkerRequest(scope: DedicatedWorkerGlobalScope, api: WorkerApi, data: unknown): Promise<void> {
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
    const method = api[data.command] as unknown;
    if (typeof method !== 'function') {
        postWorkerResponse(
            scope,
            errorResponse(
                data.id,
                workerProtocolError('WorkerProtocolError', `worker command is not implemented: ${data.command}`)
            )
        );
        return;
    }
    try {
        const packedResult = dispatchPackedCommand(api, data.command, data.args);
        const result =
            packedResult === null
                ? await (method as (...args: unknown[]) => Promise<unknown>).apply(
                      api,
                      decodeWorkerCommandPayload(data.command, data.args)
                  )
                : await packedResult;
        const responsePayload = prepareWorkerResponsePayload(data.command, result as never);
        postWorkerResponse(scope, successResponse(data.id, responsePayload.result), responsePayload.transfer);
    } catch (error) {
        postWorkerResponse(scope, errorResponse(data.id, error));
    }
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
