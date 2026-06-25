import type { WorkerApi } from './worker-api';
import type { BatchOp } from './types';
import { isRecord } from './internal';

export const WORKER_PROTOCOL_VERSION = 1;
export const WORKER_PROTOCOL_READY = 'moyodb:worker-protocol:ready';
export const WORKER_PROTOCOL_REQUEST = 'moyodb:worker-protocol:request';
export const WORKER_PROTOCOL_RESPONSE = 'moyodb:worker-protocol:response';

export const WORKER_COMMANDS = [
    'open',
    'close',
    'destroy',
    'deleteDB',
    'begin',
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
    'getIndexes',
    'reconcileIndexes',
    'listStores',
    'getVersion',
    'changesSince',
    'setSchemaVersion',
    'exportSnapshot',
    'importSnapshot',
    'reset',
    'compact',
    'rebuild',
    'stats',
    'storageInfo',
    'requestPersistence',
    'setFailpoint'
] as const;

export type WorkerCommand = (typeof WORKER_COMMANDS)[number];

type WorkerMethod<M extends WorkerCommand> = WorkerApi[M] extends (...args: infer Args) => Promise<infer Result>
    ? { args: Args; result: Result }
    : never;

export type WorkerCommandArgs<M extends WorkerCommand> = WorkerMethod<M>['args'];
export type WorkerCommandResult<M extends WorkerCommand> = WorkerMethod<M>['result'];
export type PackedBatchOpKey = {
    kind: 'put' | 'delete';
    key: Uint8Array;
};

type PreparedWorkerCommandArgs<M extends WorkerCommand> = WorkerCommandArgs<M> | unknown[];

const PACKED_BINARY_LIST_V1 = 'moyodb:packed-binary-list:v1';
const PACKED_NULLABLE_BINARY_LIST_V1 = 'moyodb:packed-nullable-binary-list:v1';
const PACKED_SCAN_ROWS_V1 = 'moyodb:packed-scan-rows:v1';
const PACKED_BATCH_OPS_V1 = 'moyodb:packed-batch-ops:v1';
const BATCH_OP_DELETE = 0;
const BATCH_OP_PUT = 1;
const U32_MAX = 0xffff_ffff;
const MAX_PACKED_RESPONSE_BYTES = 16 * 1024 * 1024;
const EMPTY_TRANSFERABLES: Transferable[] = [];

interface PackedBinaryListV1 {
    __moyodbPacked: typeof PACKED_BINARY_LIST_V1;
    bytes: Uint8Array;
}

interface PackedNullableBinaryListV1 {
    __moyodbPacked: typeof PACKED_NULLABLE_BINARY_LIST_V1;
    bytes: Uint8Array;
}

interface PackedScanRowsV1 {
    __moyodbPacked: typeof PACKED_SCAN_ROWS_V1;
    bytes: Uint8Array;
}

interface PackedBatchOpsV1 {
    __moyodbPacked: typeof PACKED_BATCH_OPS_V1;
    bytes: Uint8Array;
}

export type WorkerProtocolReadyMessage = {
    type: typeof WORKER_PROTOCOL_READY;
    version: typeof WORKER_PROTOCOL_VERSION;
};

export type WorkerProtocolRequestMessage = {
    [M in WorkerCommand]: {
        type: typeof WORKER_PROTOCOL_REQUEST;
        version: typeof WORKER_PROTOCOL_VERSION;
        id: number;
        command: M;
        args: PreparedWorkerCommandArgs<M>;
    };
}[WorkerCommand];

export interface SerializedWorkerError {
    name: string;
    code?: string;
    message: string;
    stack?: string;
}

export type WorkerProtocolSuccessMessage = {
    [M in WorkerCommand]: {
        type: typeof WORKER_PROTOCOL_RESPONSE;
        version: typeof WORKER_PROTOCOL_VERSION;
        id: number;
        ok: true;
        result: WorkerCommandResult<M>;
    };
}[WorkerCommand];

export interface WorkerProtocolErrorMessage {
    type: typeof WORKER_PROTOCOL_RESPONSE;
    version: typeof WORKER_PROTOCOL_VERSION;
    id: number;
    ok: false;
    error: SerializedWorkerError;
}

export type WorkerProtocolResponseMessage = WorkerProtocolSuccessMessage | WorkerProtocolErrorMessage;

const WORKER_COMMAND_SET = new Set<string>(WORKER_COMMANDS);

export function isWorkerCommand(value: unknown): value is WorkerCommand {
    return typeof value === 'string' && WORKER_COMMAND_SET.has(value);
}

export function isWorkerProtocolReadyMessage(value: unknown): value is WorkerProtocolReadyMessage {
    return isRecord(value) && value.type === WORKER_PROTOCOL_READY && value.version === WORKER_PROTOCOL_VERSION;
}

export function isWorkerProtocolResponseMessage(value: unknown): value is WorkerProtocolResponseMessage {
    return (
        isRecord(value) &&
        value.type === WORKER_PROTOCOL_RESPONSE &&
        value.version === WORKER_PROTOCOL_VERSION &&
        typeof value.id === 'number' &&
        Number.isSafeInteger(value.id) &&
        typeof value.ok === 'boolean'
    );
}

export function isWorkerProtocolRequestMessage(value: unknown): value is WorkerProtocolRequestMessage {
    return (
        isRecord(value) &&
        value.type === WORKER_PROTOCOL_REQUEST &&
        value.version === WORKER_PROTOCOL_VERSION &&
        typeof value.id === 'number' &&
        Number.isSafeInteger(value.id) &&
        typeof value.command === 'string' &&
        Array.isArray(value.args)
    );
}

export function isWorkerProtocolEnvelope(value: unknown): value is { type: string; id?: unknown } {
    return (
        isRecord(value) &&
        (value.type === WORKER_PROTOCOL_REQUEST ||
            value.type === WORKER_PROTOCOL_RESPONSE ||
            value.type === WORKER_PROTOCOL_READY)
    );
}

export function serializeWorkerError(error: unknown): SerializedWorkerError {
    if (isRecord(error)) {
        const code = stringOrUndefined(error.code);
        const name = stringOrUndefined(error.name) ?? code ?? 'Error';
        return {
            name,
            code,
            message: stringOrUndefined(error.message) ?? String(error),
            stack: stringOrUndefined(error.stack)
        };
    }
    return {
        name: 'Error',
        message: String(error)
    };
}

export function deserializeWorkerError(error: SerializedWorkerError): Error {
    const deserialized = new Error(error.message);
    deserialized.name = error.name || error.code || 'Error';
    if (error.stack) {
        deserialized.stack = error.stack;
    }
    if (error.code) {
        Object.defineProperty(deserialized, 'code', {
            value: error.code,
            enumerable: true,
            configurable: true
        });
    }
    return deserialized;
}

export function workerProtocolError(name: string, message: string, code = name): Error {
    const error = new Error(message);
    error.name = name;
    Object.defineProperty(error, 'code', {
        value: code,
        enumerable: true,
        configurable: true
    });
    return error;
}

export function prepareWorkerCommandPayload<M extends WorkerCommand>(
    command: M,
    args: WorkerCommandArgs<M>
): { args: PreparedWorkerCommandArgs<M>; transfer: Transferable[] } {
    if (command === 'importSnapshot') {
        const data = args[0];
        if (data instanceof Uint8Array) {
            const owned = copyToOwnedUint8Array(data);
            return {
                args: [owned] as WorkerCommandArgs<M>,
                transfer: [owned.buffer]
            };
        }
    }

    if (command === 'getMany' || command === 'deleteMany') {
        const [txId, store, keys] = args as WorkerCommandArgs<'getMany'> | WorkerCommandArgs<'deleteMany'>;
        if (Array.isArray(keys) && keys.length > 0) {
            const packed = packBinaryList(keys);
            return {
                args: [txId, store, packed],
                transfer: [packed.bytes.buffer]
            };
        }
    }

    if (command === 'putMany') {
        const [txId, store, entries, options] = args as WorkerCommandArgs<'putMany'>;
        if (Array.isArray(entries) && entries.length > 0) {
            const packed = packBinaryPairs(entries);
            return {
                args: [txId, store, packed, options],
                transfer: [packed.bytes.buffer]
            };
        }
    }

    if (command === 'applyBatch') {
        const [txId, store, ops] = args as WorkerCommandArgs<'applyBatch'>;
        if (Array.isArray(ops) && ops.length > 0) {
            const packed = packBatchOps(ops);
            return {
                args: [txId, store, packed],
                transfer: [packed.bytes.buffer]
            };
        }
    }

    return { args, transfer: EMPTY_TRANSFERABLES };
}

export function decodeWorkerCommandPayload<M extends WorkerCommand>(
    command: M,
    args: PreparedWorkerCommandArgs<M>
): WorkerCommandArgs<M> {
    if (command === 'getMany' || command === 'deleteMany') {
        const [txId, store, keys] = args as WorkerCommandArgs<'getMany'> | WorkerCommandArgs<'deleteMany'>;
        if (isPackedBinaryList(keys)) {
            return [txId, store, unpackBinaryList(keys)] as WorkerCommandArgs<M>;
        }
    }

    if (command === 'putMany') {
        const [txId, store, entries, options] = args as WorkerCommandArgs<'putMany'>;
        if (isPackedBinaryList(entries)) {
            return [txId, store, unpackBinaryPairs(entries), options] as WorkerCommandArgs<M>;
        }
    }

    if (command === 'applyBatch') {
        const [txId, store, ops] = args as WorkerCommandArgs<'applyBatch'>;
        if (isPackedBatchOps(ops)) {
            return [txId, store, unpackBatchOps(ops)] as WorkerCommandArgs<M>;
        }
    }

    return args as WorkerCommandArgs<M>;
}

export function packedBinaryListBytes(value: unknown): Uint8Array | null {
    return isPackedBinaryList(value) ? value.bytes : null;
}

export function packedBatchOpsBytes(value: unknown): Uint8Array | null {
    return isPackedBatchOps(value) ? value.bytes : null;
}

export function unpackPackedBinaryList(bytes: Uint8Array): Uint8Array[] {
    return unpackBinaryList({ __moyodbPacked: PACKED_BINARY_LIST_V1, bytes });
}

export function unpackPackedBinaryPairs(bytes: Uint8Array): Array<[Uint8Array, Uint8Array]> {
    return unpackBinaryPairs({ __moyodbPacked: PACKED_BINARY_LIST_V1, bytes });
}

export function unpackPackedBinaryPairKeys(bytes: Uint8Array): Uint8Array[] {
    return unpackBinaryPairKeys({ __moyodbPacked: PACKED_BINARY_LIST_V1, bytes });
}

export function unpackPackedBatchOps(bytes: Uint8Array): Array<BatchOp> {
    return unpackBatchOps({ __moyodbPacked: PACKED_BATCH_OPS_V1, bytes });
}

export function unpackPackedBatchOpKeys(bytes: Uint8Array): PackedBatchOpKey[] {
    return unpackBatchOpKeys({ __moyodbPacked: PACKED_BATCH_OPS_V1, bytes });
}

export function prepareWorkerResponsePayload<M extends WorkerCommand>(
    command: M,
    result: WorkerCommandResult<M>
): { result: WorkerCommandResult<M> | unknown; transfer: Transferable[] } {
    if (command === 'getMany') {
        const values = result as WorkerCommandResult<'getMany'>;
        if (Array.isArray(values) && values.length > 0) {
            const packed = packNullableBinaryList(values);
            if (packed) {
                return {
                    result: packed,
                    transfer: [packed.bytes.buffer]
                };
            }
        }
        return {
            result,
            transfer: collectTransferablesForValue(result)
        };
    }

    if (command === 'scan' || command === 'scanByIndex') {
        const rows = result as WorkerCommandResult<'scan'> | WorkerCommandResult<'scanByIndex'>;
        if (Array.isArray(rows) && rows.length > 0) {
            const packed = packScanRows(rows);
            if (packed) {
                return {
                    result: packed,
                    transfer: [packed.bytes.buffer]
                };
            }
        }
        return {
            result,
            transfer: collectTransferablesForValue(result)
        };
    }

    if (command === 'get' || command === 'getByIndex' || command === 'exportSnapshot') {
        return {
            result,
            transfer: collectDirectBinaryTransferable(result)
        };
    }

    if (command === 'changesSince') {
        return {
            result,
            transfer: collectChangeFeedTransferables(result)
        };
    }

    // The remaining command results are protocol scalars, voids, or JSON-like
    // metadata. Avoid a generic deep walk when the result type cannot contain
    // transfer-worthy binary payloads.
    return {
        result,
        transfer: []
    };
}

export function decodeWorkerResponsePayload<M extends WorkerCommand>(
    command: M,
    result: unknown
): WorkerCommandResult<M> {
    if (command === 'getMany' && isPackedNullableBinaryList(result)) {
        return unpackNullableBinaryList(result) as WorkerCommandResult<M>;
    }

    if ((command === 'scan' || command === 'scanByIndex') && isPackedScanRows(result)) {
        return unpackScanRows(result) as WorkerCommandResult<M>;
    }

    return result as WorkerCommandResult<M>;
}

export function collectTransferablesForValue(value: unknown): Transferable[] {
    const direct = directBinaryTransferable(value);
    if (direct !== null) {
        return [direct];
    }
    if (value === null || value === undefined || typeof value !== 'object') {
        return EMPTY_TRANSFERABLES;
    }

    const buffers = new Set<ArrayBuffer>();
    const seen = new WeakSet<object>();
    collectTransferableBuffers(value, buffers, seen);
    return buffers.size === 0 ? EMPTY_TRANSFERABLES : Array.from(buffers);
}

function collectDirectBinaryTransferable(value: unknown): Transferable[] {
    const direct = directBinaryTransferable(value);
    return direct === null ? EMPTY_TRANSFERABLES : [direct];
}

function directBinaryTransferable(value: unknown): ArrayBuffer | null {
    if (value instanceof ArrayBuffer) {
        return value;
    }
    if (ArrayBuffer.isView(value)) {
        const buffer = value.buffer;
        if (buffer instanceof ArrayBuffer && value.byteOffset === 0 && value.byteLength === buffer.byteLength) {
            return buffer;
        }
    }
    return null;
}

function collectChangeFeedTransferables(value: unknown): Transferable[] {
    if (!isRecord(value) || !Array.isArray(value.changes) || value.changes.length === 0) {
        return EMPTY_TRANSFERABLES;
    }

    let buffers: Set<ArrayBuffer> | null = null;
    for (const change of value.changes) {
        if (!isRecord(change)) {
            continue;
        }
        const key = directBinaryTransferable(change.key);
        if (key !== null) {
            buffers ??= new Set<ArrayBuffer>();
            buffers.add(key);
        }
        const changedValue = directBinaryTransferable(change.value);
        if (changedValue !== null) {
            buffers ??= new Set<ArrayBuffer>();
            buffers.add(changedValue);
        }
    }
    return buffers === null || buffers.size === 0 ? EMPTY_TRANSFERABLES : Array.from(buffers);
}

function collectTransferableBuffers(value: unknown, buffers: Set<ArrayBuffer>, seen: WeakSet<object>): void {
    if (value === null || value === undefined) {
        return;
    }
    if (value instanceof ArrayBuffer) {
        buffers.add(value);
        return;
    }
    if (ArrayBuffer.isView(value)) {
        const buffer = value.buffer;
        if (buffer instanceof ArrayBuffer && value.byteOffset === 0 && value.byteLength === buffer.byteLength) {
            buffers.add(buffer);
        }
        return;
    }
    if (typeof value !== 'object') {
        return;
    }
    const object = value as object;
    if (seen.has(object)) {
        return;
    }
    seen.add(object);
    if (Array.isArray(value)) {
        for (const item of value) {
            collectTransferableBuffers(item, buffers, seen);
        }
        return;
    }
    const record = value as Record<string, unknown>;
    for (const key in record) {
        if (Object.prototype.hasOwnProperty.call(record, key)) {
            collectTransferableBuffers(record[key], buffers, seen);
        }
    }
}

function packBinaryPairs(entries: Array<[Uint8Array, Uint8Array]>): PackedBinaryListV1 {
    const entryCount = entries.length;
    const count = checkedU32(entryCount * 2, 'packed putMany item count');
    const metadataBytes = checkedByteCount(count, 4, 'packed putMany metadata');
    let payloadBytes = 0;
    for (let index = 0; index < entryCount; index += 1) {
        const entry = entries[index]!;
        const key = entry[0];
        const value = entry[1];
        if (!(key instanceof Uint8Array) || !(value instanceof Uint8Array)) {
            throw workerProtocolError('WorkerProtocolError', 'putMany entry key/value is not a Uint8Array');
        }
        const keyLength = checkedU32(key.byteLength, 'putMany key length');
        const valueLength = checkedU32(value.byteLength, 'putMany value length');
        payloadBytes += keyLength + valueLength;
        if (payloadBytes > U32_MAX) {
            throw workerProtocolError('WorkerProtocolError', 'packed putMany payload exceeds 32-bit payload limit');
        }
    }
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed putMany header');
    const totalBytes = checkedAdd(payloadOffset, payloadBytes, 'packed putMany total');
    const bytes = new Uint8Array(totalBytes);
    const view = new DataView(bytes.buffer);
    view.setUint32(0, count, true);
    let metadataOffset = 4;
    let writeOffset = payloadOffset;
    for (let index = 0; index < entryCount; index += 1) {
        const entry = entries[index]!;
        const key = entry[0];
        const value = entry[1];
        const keyLength = key.byteLength;
        const valueLength = value.byteLength;
        view.setUint32(metadataOffset, keyLength, true);
        metadataOffset += 4;
        view.setUint32(metadataOffset, valueLength, true);
        metadataOffset += 4;
        bytes.set(key, writeOffset);
        writeOffset += keyLength;
        bytes.set(value, writeOffset);
        writeOffset += valueLength;
    }
    return {
        __moyodbPacked: PACKED_BINARY_LIST_V1,
        bytes
    };
}

function packBinaryList(items: Uint8Array[]): PackedBinaryListV1 {
    const count = items.length;
    const metadataBytes = checkedByteCount(count, 4, 'packed binary-list metadata');
    let payloadBytes = 0;
    for (let index = 0; index < count; index += 1) {
        const item = items[index]!;
        if (!(item instanceof Uint8Array)) {
            throw workerProtocolError('WorkerProtocolError', 'binary-list payload item is not a Uint8Array');
        }
        const itemLength = checkedU32(item.byteLength, 'binary-list item length');
        payloadBytes += itemLength;
        if (payloadBytes > U32_MAX) {
            throw workerProtocolError('WorkerProtocolError', 'packed binary-list payload exceeds 32-bit payload limit');
        }
    }
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed binary-list header');
    const totalBytes = checkedAdd(payloadOffset, payloadBytes, 'packed binary-list total');
    const bytes = new Uint8Array(totalBytes);
    const view = new DataView(bytes.buffer);
    view.setUint32(0, count, true);
    let metadataOffset = 4;
    let writeOffset = payloadOffset;
    for (let index = 0; index < count; index += 1) {
        const item = items[index]!;
        const itemLength = item.byteLength;
        view.setUint32(metadataOffset, itemLength, true);
        metadataOffset += 4;
        bytes.set(item, writeOffset);
        writeOffset += itemLength;
    }
    return {
        __moyodbPacked: PACKED_BINARY_LIST_V1,
        bytes
    };
}

function unpackBinaryList(payload: PackedBinaryListV1): Uint8Array[] {
    const bytes = payload.bytes;
    if (!(bytes instanceof Uint8Array) || bytes.byteLength < 4) {
        throw workerProtocolError('WorkerProtocolError', 'invalid packed binary-list payload');
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const count = view.getUint32(0, true);
    const metadataBytes = checkedByteCount(count, 4, 'packed binary-list metadata');
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed binary-list header');
    if (payloadOffset > bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed binary-list metadata exceeds payload length');
    }

    const items = new Array<Uint8Array>(count);
    let metadataOffset = 4;
    let readOffset = payloadOffset;
    for (let index = 0; index < count; index += 1) {
        const byteLength = view.getUint32(metadataOffset, true);
        metadataOffset += 4;
        const end = readOffset + byteLength;
        if (end > U32_MAX || end > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed binary-list item exceeds payload length');
        }
        items[index] = bytes.subarray(readOffset, end);
        readOffset = end;
    }
    if (readOffset !== bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed binary-list payload has trailing bytes');
    }
    return items;
}

function unpackBinaryPairs(payload: PackedBinaryListV1): Array<[Uint8Array, Uint8Array]> {
    const bytes = payload.bytes;
    if (!(bytes instanceof Uint8Array) || bytes.byteLength < 4) {
        throw workerProtocolError('WorkerProtocolError', 'invalid packed putMany payload');
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const itemCount = view.getUint32(0, true);
    if (itemCount % 2 !== 0) {
        throw workerProtocolError('WorkerProtocolError', 'packed putMany payload has an odd item count');
    }
    const metadataBytes = checkedByteCount(itemCount, 4, 'packed putMany metadata');
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed putMany header');
    if (payloadOffset > bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed putMany metadata exceeds payload length');
    }

    const entryCount = itemCount / 2;
    const entries = new Array<[Uint8Array, Uint8Array]>(entryCount);
    let metadataOffset = 4;
    let readOffset = payloadOffset;
    for (let index = 0; index < entryCount; index += 1) {
        const keyLength = view.getUint32(metadataOffset, true);
        const valueLength = view.getUint32(metadataOffset + 4, true);
        metadataOffset += 8;

        const keyEnd = readOffset + keyLength;
        if (keyEnd > U32_MAX || keyEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed putMany key exceeds payload length');
        }
        const valueEnd = keyEnd + valueLength;
        if (valueEnd > U32_MAX || valueEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed putMany value exceeds payload length');
        }
        entries[index] = [bytes.subarray(readOffset, keyEnd), bytes.subarray(keyEnd, valueEnd)];
        readOffset = valueEnd;
    }
    if (readOffset !== bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed putMany payload has trailing bytes');
    }
    return entries;
}

function unpackBinaryPairKeys(payload: PackedBinaryListV1): Uint8Array[] {
    const bytes = payload.bytes;
    if (!(bytes instanceof Uint8Array) || bytes.byteLength < 4) {
        throw workerProtocolError('WorkerProtocolError', 'invalid packed putMany payload');
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const itemCount = view.getUint32(0, true);
    if (itemCount % 2 !== 0) {
        throw workerProtocolError('WorkerProtocolError', 'packed putMany payload has an odd item count');
    }
    const metadataBytes = checkedByteCount(itemCount, 4, 'packed putMany metadata');
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed putMany header');
    if (payloadOffset > bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed putMany metadata exceeds payload length');
    }

    const entryCount = itemCount / 2;
    const keys = new Array<Uint8Array>(entryCount);
    let metadataOffset = 4;
    let readOffset = payloadOffset;
    for (let index = 0; index < entryCount; index += 1) {
        const keyLength = view.getUint32(metadataOffset, true);
        const valueLength = view.getUint32(metadataOffset + 4, true);
        metadataOffset += 8;

        const keyEnd = readOffset + keyLength;
        if (keyEnd > U32_MAX || keyEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed putMany key exceeds payload length');
        }
        const valueEnd = keyEnd + valueLength;
        if (valueEnd > U32_MAX || valueEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed putMany value exceeds payload length');
        }
        keys[index] = bytes.subarray(readOffset, keyEnd);
        readOffset = valueEnd;
    }
    if (readOffset !== bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed putMany payload has trailing bytes');
    }
    return keys;
}

function packNullableBinaryList(items: Array<Uint8Array | null>): PackedNullableBinaryListV1 | null {
    const count = items.length;
    const metadataBytes = checkedByteCount(count, 5, 'packed nullable binary-list metadata');
    let payloadBytes = 0;
    for (let index = 0; index < count; index += 1) {
        const item = items[index];
        if (item === null) {
            continue;
        }
        if (!(item instanceof Uint8Array)) {
            throw workerProtocolError(
                'WorkerProtocolError',
                'nullable binary-list payload item is not a Uint8Array or null'
            );
        }
        const itemLength = checkedU32(item.byteLength, 'nullable binary-list item length');
        payloadBytes += itemLength;
        if (payloadBytes > U32_MAX) {
            throw workerProtocolError(
                'WorkerProtocolError',
                'packed nullable binary-list payload exceeds 32-bit payload limit'
            );
        }
    }
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed nullable binary-list header');
    const totalBytes = checkedAdd(payloadOffset, payloadBytes, 'packed nullable binary-list total');
    if (totalBytes > MAX_PACKED_RESPONSE_BYTES) {
        return null;
    }
    const bytes = new Uint8Array(totalBytes);
    const view = new DataView(bytes.buffer);
    view.setUint32(0, count, true);
    let metadataOffset = 4;
    let writeOffset = payloadOffset;
    for (let index = 0; index < count; index += 1) {
        const item = items[index];
        if (item === null) {
            view.setUint8(metadataOffset, 0);
            view.setUint32(metadataOffset + 1, 0, true);
            metadataOffset += 5;
            continue;
        }
        const itemLength = item.byteLength;
        view.setUint8(metadataOffset, 1);
        view.setUint32(metadataOffset + 1, itemLength, true);
        metadataOffset += 5;
        bytes.set(item, writeOffset);
        writeOffset += itemLength;
    }
    return {
        __moyodbPacked: PACKED_NULLABLE_BINARY_LIST_V1,
        bytes
    };
}

function unpackNullableBinaryList(payload: PackedNullableBinaryListV1): Array<Uint8Array | null> {
    const bytes = payload.bytes;
    if (!(bytes instanceof Uint8Array) || bytes.byteLength < 4) {
        throw workerProtocolError('WorkerProtocolError', 'invalid packed nullable binary-list payload');
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const count = view.getUint32(0, true);
    const metadataBytes = checkedByteCount(count, 5, 'packed nullable binary-list metadata');
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed nullable binary-list header');
    if (payloadOffset > bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed nullable binary-list metadata exceeds payload length');
    }

    const items = new Array<Uint8Array | null>(count);
    let metadataOffset = 4;
    let readOffset = payloadOffset;
    for (let index = 0; index < count; index += 1) {
        const present = view.getUint8(metadataOffset);
        const byteLength = view.getUint32(metadataOffset + 1, true);
        metadataOffset += 5;
        if (present === 0) {
            if (byteLength !== 0) {
                throw workerProtocolError(
                    'WorkerProtocolError',
                    'packed nullable binary-list null item has a payload length'
                );
            }
            items[index] = null;
            continue;
        }
        if (present !== 1) {
            throw workerProtocolError(
                'WorkerProtocolError',
                `packed nullable binary-list item has invalid presence byte: ${present}`
            );
        }
        const end = readOffset + byteLength;
        if (end > U32_MAX || end > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed nullable binary-list item exceeds payload length');
        }
        items[index] = bytes.subarray(readOffset, end);
        readOffset = end;
    }
    if (readOffset !== bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed nullable binary-list payload has trailing bytes');
    }
    return items;
}

function packScanRows(rows: Array<{ key: Uint8Array; value: Uint8Array }>): PackedScanRowsV1 | null {
    const count = rows.length;
    const metadataBytes = checkedByteCount(count, 8, 'packed scan row metadata');
    let payloadBytes = 0;
    for (let index = 0; index < count; index += 1) {
        const row = rows[index]!;
        if (!isRecord(row) || !(row.key instanceof Uint8Array) || !(row.value instanceof Uint8Array)) {
            throw workerProtocolError('WorkerProtocolError', 'scan row payload key/value is not a Uint8Array');
        }
        const keyLength = checkedU32(row.key.byteLength, 'scan row key length');
        const valueLength = checkedU32(row.value.byteLength, 'scan row value length');
        payloadBytes += keyLength + valueLength;
        if (payloadBytes > U32_MAX) {
            throw workerProtocolError('WorkerProtocolError', 'packed scan row payload exceeds 32-bit payload limit');
        }
    }
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed scan row header');
    const totalBytes = checkedAdd(payloadOffset, payloadBytes, 'packed scan row total');
    if (totalBytes > MAX_PACKED_RESPONSE_BYTES) {
        return null;
    }
    const bytes = new Uint8Array(totalBytes);
    const view = new DataView(bytes.buffer);
    view.setUint32(0, count, true);
    let metadataOffset = 4;
    let writeOffset = payloadOffset;
    for (let index = 0; index < count; index += 1) {
        const row = rows[index]!;
        const key = row.key;
        const value = row.value;
        const keyLength = key.byteLength;
        const valueLength = value.byteLength;
        view.setUint32(metadataOffset, keyLength, true);
        view.setUint32(metadataOffset + 4, valueLength, true);
        metadataOffset += 8;
        bytes.set(key, writeOffset);
        writeOffset += keyLength;
        bytes.set(value, writeOffset);
        writeOffset += valueLength;
    }
    return {
        __moyodbPacked: PACKED_SCAN_ROWS_V1,
        bytes
    };
}

function unpackScanRows(payload: PackedScanRowsV1): Array<{ key: Uint8Array; value: Uint8Array }> {
    const bytes = payload.bytes;
    if (!(bytes instanceof Uint8Array) || bytes.byteLength < 4) {
        throw workerProtocolError('WorkerProtocolError', 'invalid packed scan row payload');
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const count = view.getUint32(0, true);
    const metadataBytes = checkedByteCount(count, 8, 'packed scan row metadata');
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed scan row header');
    if (payloadOffset > bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed scan row metadata exceeds payload length');
    }

    const rows = new Array<{ key: Uint8Array; value: Uint8Array }>(count);
    let metadataOffset = 4;
    let readOffset = payloadOffset;
    for (let index = 0; index < count; index += 1) {
        const keyLength = view.getUint32(metadataOffset, true);
        const valueLength = view.getUint32(metadataOffset + 4, true);
        metadataOffset += 8;
        const keyEnd = readOffset + keyLength;
        if (keyEnd > U32_MAX || keyEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed scan row key exceeds payload length');
        }
        const valueEnd = keyEnd + valueLength;
        if (valueEnd > U32_MAX || valueEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed scan row value exceeds payload length');
        }
        rows[index] = {
            key: bytes.subarray(readOffset, keyEnd),
            value: bytes.subarray(keyEnd, valueEnd)
        };
        readOffset = valueEnd;
    }
    if (readOffset !== bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed scan row payload has trailing bytes');
    }
    return rows;
}

function packBatchOps(ops: Array<BatchOp>): PackedBatchOpsV1 {
    const count = ops.length;
    const metadataBytes = checkedByteCount(count, 9, 'packed batch metadata');
    let payloadBytes = 0;
    for (let index = 0; index < count; index += 1) {
        const op = ops[index]!;
        if (op.kind !== 'put' && op.kind !== 'delete') {
            throw workerProtocolError(
                'WorkerProtocolError',
                `unknown batch operation kind: ${String((op as { kind?: unknown }).kind)}`
            );
        }
        if (!(op.key instanceof Uint8Array)) {
            throw workerProtocolError('WorkerProtocolError', 'batch operation key is not a Uint8Array');
        }
        const keyLength = checkedU32(op.key.byteLength, 'batch operation key length');
        payloadBytes += keyLength;
        if (payloadBytes > U32_MAX) {
            throw workerProtocolError('WorkerProtocolError', 'packed batch payload exceeds 32-bit payload limit');
        }
        if (op.kind === 'put') {
            if (!(op.value instanceof Uint8Array)) {
                throw workerProtocolError('WorkerProtocolError', 'batch put value is not a Uint8Array');
            }
            const valueLength = checkedU32(op.value.byteLength, 'batch put value length');
            payloadBytes += valueLength;
            if (payloadBytes > U32_MAX) {
                throw workerProtocolError('WorkerProtocolError', 'packed batch payload exceeds 32-bit payload limit');
            }
        }
    }
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed batch header');
    const totalBytes = checkedAdd(payloadOffset, payloadBytes, 'packed batch total');
    const bytes = new Uint8Array(totalBytes);
    const view = new DataView(bytes.buffer);
    view.setUint32(0, count, true);
    let metadataOffset = 4;
    let writeOffset = payloadOffset;
    for (let index = 0; index < count; index += 1) {
        const op = ops[index]!;
        const key = op.key;
        const keyLength = key.byteLength;
        const valueLength = op.kind === 'put' ? op.value.byteLength : 0;
        view.setUint8(metadataOffset, op.kind === 'put' ? BATCH_OP_PUT : BATCH_OP_DELETE);
        view.setUint32(metadataOffset + 1, keyLength, true);
        view.setUint32(metadataOffset + 5, valueLength, true);
        metadataOffset += 9;
        bytes.set(key, writeOffset);
        writeOffset += keyLength;
        if (op.kind === 'put') {
            bytes.set(op.value, writeOffset);
            writeOffset += valueLength;
        }
    }
    return {
        __moyodbPacked: PACKED_BATCH_OPS_V1,
        bytes
    };
}

function unpackBatchOps(payload: PackedBatchOpsV1): Array<BatchOp> {
    const bytes = payload.bytes;
    if (!(bytes instanceof Uint8Array) || bytes.byteLength < 4) {
        throw workerProtocolError('WorkerProtocolError', 'invalid packed batch payload');
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const count = view.getUint32(0, true);
    const metadataBytes = checkedByteCount(count, 9, 'packed batch metadata');
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed batch header');
    if (payloadOffset > bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed batch metadata exceeds payload length');
    }

    const ops = new Array<BatchOp>(count);
    let metadataOffset = 4;
    let readOffset = payloadOffset;
    for (let index = 0; index < count; index += 1) {
        const kind = view.getUint8(metadataOffset);
        const keyLength = view.getUint32(metadataOffset + 1, true);
        const valueLength = view.getUint32(metadataOffset + 5, true);
        metadataOffset += 9;

        const keyEnd = readOffset + keyLength;
        if (keyEnd > U32_MAX || keyEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed batch key exceeds payload length');
        }
        const key = bytes.subarray(readOffset, keyEnd);
        readOffset = keyEnd;

        if (kind === BATCH_OP_DELETE) {
            if (valueLength !== 0) {
                throw workerProtocolError('WorkerProtocolError', 'packed delete operation has a value payload');
            }
            ops[index] = { kind: 'delete', key };
            continue;
        }

        if (kind !== BATCH_OP_PUT) {
            throw workerProtocolError('WorkerProtocolError', `packed batch operation has invalid kind byte: ${kind}`);
        }
        const valueEnd = readOffset + valueLength;
        if (valueEnd > U32_MAX || valueEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed batch value exceeds payload length');
        }
        ops[index] = { kind: 'put', key, value: bytes.subarray(readOffset, valueEnd) };
        readOffset = valueEnd;
    }
    if (readOffset !== bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed batch payload has trailing bytes');
    }
    return ops;
}

function unpackBatchOpKeys(payload: PackedBatchOpsV1): PackedBatchOpKey[] {
    const bytes = payload.bytes;
    if (!(bytes instanceof Uint8Array) || bytes.byteLength < 4) {
        throw workerProtocolError('WorkerProtocolError', 'invalid packed batch payload');
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const count = view.getUint32(0, true);
    const metadataBytes = checkedByteCount(count, 9, 'packed batch metadata');
    const payloadOffset = checkedAdd(4, metadataBytes, 'packed batch header');
    if (payloadOffset > bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed batch metadata exceeds payload length');
    }

    const ops = new Array<PackedBatchOpKey>(count);
    let metadataOffset = 4;
    let readOffset = payloadOffset;
    for (let index = 0; index < count; index += 1) {
        const kind = view.getUint8(metadataOffset);
        const keyLength = view.getUint32(metadataOffset + 1, true);
        const valueLength = view.getUint32(metadataOffset + 5, true);
        metadataOffset += 9;

        const keyEnd = readOffset + keyLength;
        if (keyEnd > U32_MAX || keyEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed batch key exceeds payload length');
        }
        const key = bytes.subarray(readOffset, keyEnd);
        readOffset = keyEnd;

        if (kind === BATCH_OP_DELETE) {
            if (valueLength !== 0) {
                throw workerProtocolError('WorkerProtocolError', 'packed delete operation has a value payload');
            }
            ops[index] = { kind: 'delete', key };
            continue;
        }

        if (kind !== BATCH_OP_PUT) {
            throw workerProtocolError('WorkerProtocolError', `packed batch operation has invalid kind byte: ${kind}`);
        }
        const valueEnd = readOffset + valueLength;
        if (valueEnd > U32_MAX || valueEnd > bytes.byteLength) {
            throw workerProtocolError('WorkerProtocolError', 'packed batch value exceeds payload length');
        }
        ops[index] = { kind: 'put', key };
        readOffset = valueEnd;
    }
    if (readOffset !== bytes.byteLength) {
        throw workerProtocolError('WorkerProtocolError', 'packed batch payload has trailing bytes');
    }
    return ops;
}

function isPackedBinaryList(value: unknown): value is PackedBinaryListV1 {
    return isRecord(value) && value.__moyodbPacked === PACKED_BINARY_LIST_V1 && value.bytes instanceof Uint8Array;
}

function isPackedNullableBinaryList(value: unknown): value is PackedNullableBinaryListV1 {
    return (
        isRecord(value) && value.__moyodbPacked === PACKED_NULLABLE_BINARY_LIST_V1 && value.bytes instanceof Uint8Array
    );
}

function isPackedScanRows(value: unknown): value is PackedScanRowsV1 {
    return isRecord(value) && value.__moyodbPacked === PACKED_SCAN_ROWS_V1 && value.bytes instanceof Uint8Array;
}

function isPackedBatchOps(value: unknown): value is PackedBatchOpsV1 {
    return isRecord(value) && value.__moyodbPacked === PACKED_BATCH_OPS_V1 && value.bytes instanceof Uint8Array;
}

function checkedByteCount(count: number, itemBytes: number, what: string): number {
    if (!Number.isSafeInteger(count) || count < 0) {
        throw workerProtocolError('WorkerProtocolError', `${what} count is invalid: ${count}`);
    }
    return checkedU32(count * itemBytes, what);
}

function checkedAdd(left: number, right: number, what: string): number {
    const total = left + right;
    return checkedU32(total, what);
}

function checkedU32(value: number, what: string): number {
    if (!Number.isSafeInteger(value) || value < 0 || value > U32_MAX) {
        throw workerProtocolError('WorkerProtocolError', `${what} exceeds 32-bit payload limit`);
    }
    return value;
}

function copyToOwnedUint8Array(value: Uint8Array): Uint8Array {
    const owned = new Uint8Array(value.byteLength);
    owned.set(value);
    return owned;
}

function stringOrUndefined(value: unknown): string | undefined {
    return typeof value === 'string' ? value : undefined;
}
