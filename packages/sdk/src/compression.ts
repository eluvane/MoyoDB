import { utf8Encode } from './codec';
import { decodeSnappy, encodeSnappy } from './snappy';
import type { CompressionKind, SnapshotCompressionKind } from './types';
export type CompressionOption = CompressionKind | false;
const STORE_RECORD_MAGIC = utf8Encode('BDBZVAL1');
const SNAPSHOT_EXPORT_MAGIC = utf8Encode('BDBZSNP1');
const ENVELOPE_VERSION = 1;
const ENVELOPE_HEADER_SIZE = 18;
const COMPRESSION_TAG_NONE = 0;
const COMPRESSION_TAG_GZIP = 1;
const COMPRESSION_TAG_DEFLATE = 2;
const COMPRESSION_TAG_SNAPPY = 3;
const STORE_FLAG_COMPRESSION_SHIFT = 2;
const STORE_FLAG_COMPRESSION_MASK = 0b11 << STORE_FLAG_COMPRESSION_SHIFT;
export const STORE_VALUE_COMPRESSION_THRESHOLD = 1024;
export const STORE_VALUE_MIN_COMPRESSION_SAVING_PERCENT = 10;
export const STORE_VALUE_COMPRESSION_POLICY_VERSION = 2;
export const STORE_VALUE_COMPRESSION_PREFLIGHT_MIN_BYTES = 64 * 1024;
export const STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_BYTES = 1024;
export const STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_COUNT = 3;
export const STORE_VALUE_COMPRESSION_ENVELOPE_BYTES = ENVELOPE_HEADER_SIZE;
const MAX_DECOMPRESSED_STORE_VALUE_BYTES = 8 * 1024 * 1024;
const MAX_DECOMPRESSED_SNAPSHOT_BYTES = 256 * 1024 * 1024;
const CRC32_TABLE = (() => {
    const table = new Uint32Array(8 * 256);
    for (let index = 0; index < 256; index += 1) {
        let crc = index;
        for (let bit = 0; bit < 8; bit += 1) {
            if ((crc & 1) !== 0) {
                crc = (crc >>> 1) ^ 0xedb88320;
            } else {
                crc >>>= 1;
            }
        }
        table[index] = crc >>> 0;
    }
    for (let index = 0; index < 256; index += 1) {
        let crc = table[index];
        for (let slice = 1; slice < 8; slice += 1) {
            crc = (crc >>> 8) ^ table[crc & 0xff];
            table[slice * 256 + index] = crc >>> 0;
        }
    }
    return table;
})();
type EnvelopeHeader = {
    version: number;
    kindTag: number;
    rawLength: number;
    payloadChecksum: number;
};
export type PreparedStoreValueRecord = {
    readonly byteLength: number;
    readonly payload: Uint8Array;
    readonly rawLength: number;
    readonly kindTag: number | null;
};
function namedError(name: string, message: string): Error {
    const error = new Error(message);
    error.name = name;
    return error;
}
function compressionTag(kind: CompressionOption): number {
    switch (kind) {
        case false:
            return COMPRESSION_TAG_NONE;
        case 'gzip':
            return COMPRESSION_TAG_GZIP;
        case 'deflate':
            return COMPRESSION_TAG_DEFLATE;
        case 'snappy':
            return COMPRESSION_TAG_SNAPPY;
        default:
            throw namedError('InternalError', `unsupported compression kind: ${String(kind)}`);
    }
}
function compressionKindFromTag(tag: number): CompressionOption {
    switch (tag) {
        case COMPRESSION_TAG_NONE:
            return false;
        case COMPRESSION_TAG_GZIP:
            return 'gzip';
        case COMPRESSION_TAG_DEFLATE:
            return 'deflate';
        case COMPRESSION_TAG_SNAPPY:
            return 'snappy';
        default:
            throw namedError('CorruptionError', `invalid compression tag: ${tag}`);
    }
}
function compressionRuntimeUnavailable(): Error {
    return namedError(
        'UnsupportedPlatformError',
        'CompressionStream and DecompressionStream are required for moyodb compression'
    );
}
function ensureCompressionRuntime(): void {
    if (typeof CompressionStream !== 'function' || typeof DecompressionStream !== 'function') {
        throw compressionRuntimeUnavailable();
    }
}
function crc32(bytes: Uint8Array): number {
    let crc = 0xffffffff;
    let offset = 0;
    const blockEnd = bytes.byteLength - 7;
    // Table slices apply eight IEEE CRC32 byte updates in one step.
    for (; offset < blockEnd; offset += 8) {
        const first =
            crc ^ bytes[offset] ^ (bytes[offset + 1] << 8) ^ (bytes[offset + 2] << 16) ^ (bytes[offset + 3] << 24);
        crc =
            CRC32_TABLE[1792 + (first & 0xff)] ^
            CRC32_TABLE[1536 + ((first >>> 8) & 0xff)] ^
            CRC32_TABLE[1280 + ((first >>> 16) & 0xff)] ^
            CRC32_TABLE[1024 + (first >>> 24)] ^
            CRC32_TABLE[768 + bytes[offset + 4]] ^
            CRC32_TABLE[512 + bytes[offset + 5]] ^
            CRC32_TABLE[256 + bytes[offset + 6]] ^
            CRC32_TABLE[bytes[offset + 7]];
    }
    for (; offset < bytes.byteLength; offset += 1) {
        crc = (crc >>> 8) ^ CRC32_TABLE[(crc ^ bytes[offset]) & 0xff];
    }
    return (crc ^ 0xffffffff) >>> 0;
}
function buildEnvelope(magic: Uint8Array, kindTag: number, rawLength: number, payload: Uint8Array): Uint8Array {
    const out = new Uint8Array(ENVELOPE_HEADER_SIZE + payload.byteLength);
    writeEnvelope(out, 0, magic, kindTag, rawLength, payload);
    return out;
}
function writeEnvelope(
    out: Uint8Array,
    offset: number,
    magic: Uint8Array,
    kindTag: number,
    rawLength: number,
    payload: Uint8Array
): void {
    // A borrowed payload can overlap the destination header.
    const checksum = crc32(payload);
    out.set(payload, offset + ENVELOPE_HEADER_SIZE);
    out.set(magic, offset);
    out[offset + 8] = ENVELOPE_VERSION;
    out[offset + 9] = kindTag;
    const view = new DataView(out.buffer, out.byteOffset + offset, ENVELOPE_HEADER_SIZE);
    view.setUint32(10, rawLength, true);
    view.setUint32(14, checksum, true);
}
function writeEnvelopeHeader(
    out: Uint8Array,
    magic: Uint8Array,
    kindTag: number,
    rawLength: number,
    payload: Uint8Array
): void {
    const checksum = crc32(payload);
    out.set(magic);
    out[8] = ENVELOPE_VERSION;
    out[9] = kindTag;
    const view = new DataView(out.buffer, out.byteOffset, ENVELOPE_HEADER_SIZE);
    view.setUint32(10, rawLength, true);
    view.setUint32(14, checksum, true);
}
function envelopeFromChunks(
    magic: Uint8Array,
    kindTag: number,
    rawLength: number,
    chunks: Uint8Array[],
    totalBytes: number
): Uint8Array {
    const out = new Uint8Array(ENVELOPE_HEADER_SIZE + totalBytes);
    let offset = ENVELOPE_HEADER_SIZE;
    for (const chunk of chunks) {
        out.set(chunk, offset);
        offset += chunk.byteLength;
    }
    writeEnvelopeHeader(out, magic, kindTag, rawLength, out.subarray(ENVELOPE_HEADER_SIZE));
    return out;
}
function compressionIsProfitable(rawLength: number, compressedLength: number): boolean {
    return (
        (compressedLength + ENVELOPE_HEADER_SIZE) * 100 <=
        rawLength * (100 - STORE_VALUE_MIN_COMPRESSION_SAVING_PERCENT)
    );
}
function snappyPreflightIsProfitable(value: Uint8Array): boolean {
    if (value.byteLength < STORE_VALUE_COMPRESSION_PREFLIGHT_MIN_BYTES) {
        return true;
    }
    let compressedLength = 0;
    for (let index = 0; index < STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_COUNT; index += 1) {
        const offset = Math.floor(
            ((value.byteLength - STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_BYTES) * index) /
                (STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_COUNT - 1)
        );
        compressedLength += encodeSnappy(
            value.subarray(offset, offset + STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_BYTES)
        ).byteLength;
    }
    return compressionIsProfitable(
        STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_BYTES * STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_COUNT,
        compressedLength
    );
}
function validateStoreValueLength(value: Uint8Array): void {
    if (value.byteLength > MAX_DECOMPRESSED_STORE_VALUE_BYTES) {
        throw namedError(
            'ValueTooLargeError',
            `value has ${value.byteLength} bytes, exceeding the ${MAX_DECOMPRESSED_STORE_VALUE_BYTES} byte limit`
        );
    }
}
// Raw payloads borrow value until the prepared record is written.
export function prepareFastStoreValueRecord(
    value: Uint8Array,
    compression: 'snappy' | false
): PreparedStoreValueRecord {
    if (compression === false) {
        return { payload: value, rawLength: value.byteLength, byteLength: value.byteLength, kindTag: null };
    }
    validateStoreValueLength(value);
    let payload = value;
    let kindTag = COMPRESSION_TAG_NONE;
    if (value.byteLength >= STORE_VALUE_COMPRESSION_THRESHOLD && snappyPreflightIsProfitable(value)) {
        const compressed = encodeSnappy(value);
        if (compressionIsProfitable(value.byteLength, compressed.byteLength)) {
            payload = compressed;
            kindTag = COMPRESSION_TAG_SNAPPY;
        }
    }
    return { payload, rawLength: value.byteLength, byteLength: ENVELOPE_HEADER_SIZE + payload.byteLength, kindTag };
}
export function writePreparedStoreValueRecord(
    record: PreparedStoreValueRecord,
    target: Uint8Array,
    offset: number
): number {
    if (!Number.isSafeInteger(offset) || offset < 0 || record.byteLength > target.byteLength - offset) {
        throw namedError('InternalError', 'prepared store value exceeds its destination');
    }
    if (record.kindTag === null) {
        target.set(record.payload, offset);
    } else {
        writeEnvelope(target, offset, STORE_RECORD_MAGIC, record.kindTag, record.rawLength, record.payload);
    }
    return offset + record.byteLength;
}
// Copy before the stream can suspend so caller mutations cannot change its input.
function copyBytes(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
    const copy = new Uint8Array(bytes.byteLength);
    copy.set(bytes);
    return copy;
}
function byteStream(bytes: Uint8Array<ArrayBuffer>): ReadableStream<Uint8Array<ArrayBuffer>> {
    let offset = 0;
    return new ReadableStream({
        pull(controller) {
            if (offset < bytes.byteLength) {
                const end = Math.min(offset + 64 * 1024, bytes.byteLength);
                // The stream owns the input; chunk views need no further copies.
                controller.enqueue(bytes.subarray(offset, end));
                offset = end;
            }
            if (offset === bytes.byteLength) {
                controller.close();
            }
        }
    });
}
function readEnvelopeHeader(magic: Uint8Array, bytes: Uint8Array, strict = true): EnvelopeHeader | null {
    if (bytes.byteLength < magic.byteLength) {
        return null;
    }
    for (let index = 0; index < magic.byteLength; index += 1) {
        if (bytes[index] !== magic[index]) {
            return null;
        }
    }
    if (bytes.byteLength < ENVELOPE_HEADER_SIZE) {
        if (strict) {
            throw namedError(
                'CorruptionError',
                `truncated compression envelope: expected at least ${ENVELOPE_HEADER_SIZE} bytes, got ${bytes.byteLength}`
            );
        }
        return null;
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, ENVELOPE_HEADER_SIZE);
    return {
        version: bytes[8],
        kindTag: bytes[9],
        rawLength: view.getUint32(10, true),
        payloadChecksum: view.getUint32(14, true)
    };
}
// `stopAtBytes` abandons a result that will not be smaller than the raw payload.
// The caller then seals the snapshot it already owns instead of copying those bytes again.
async function collectCompressed(
    input: Uint8Array<ArrayBuffer>,
    kind: SnapshotCompressionKind,
    stopAtBytes?: number
): Promise<{
    chunks: Uint8Array[];
    totalBytes: number;
} | null> {
    ensureCompressionRuntime();
    let stream: ReadableStream<Uint8Array>;
    try {
        stream = byteStream(input).pipeThrough(new CompressionStream(kind));
    } catch {
        throw compressionRuntimeUnavailable();
    }
    const reader = stream.getReader();
    const chunks: Uint8Array[] = [];
    let totalBytes = 0;
    let stopped = false;
    try {
        for (let next = await reader.read(); !next.done; next = await reader.read()) {
            const part = next.value;
            totalBytes += part.byteLength;
            if (stopAtBytes !== undefined && totalBytes >= stopAtBytes) {
                stopped = true;
                await reader.cancel('compressed output is not smaller than the raw payload');
                break;
            }
            chunks.push(part);
        }
    } catch (error) {
        if (!stopped) {
            throw namedError('InternalError', `failed to compress bytes with ${kind}: ${String(error)}`);
        }
    } finally {
        try {
            reader.releaseLock();
        } catch {
            // cancel() may already have released this reader.
        }
    }
    if (stopped || (stopAtBytes !== undefined && totalBytes >= stopAtBytes)) {
        return null;
    }
    return { chunks, totalBytes };
}
async function decompressBytes(
    data: Uint8Array,
    kind: CompressionKind,
    label: string,
    maxOutputBytes?: number
): Promise<Uint8Array> {
    if (kind === 'snappy') {
        return decodeSnappy(data, maxOutputBytes ?? MAX_DECOMPRESSED_STORE_VALUE_BYTES, maxOutputBytes);
    }
    ensureCompressionRuntime();
    let stream: ReadableStream<Uint8Array>;
    try {
        stream = byteStream(copyBytes(data)).pipeThrough(new DecompressionStream(kind));
    } catch {
        throw compressionRuntimeUnavailable();
    }
    try {
        return await readBoundedStream(stream, label, maxOutputBytes);
    } catch (error) {
        throw namedError('CorruptionError', `failed to decompress ${label}: ${String(error)}`);
    }
}

async function readBoundedStream(
    stream: ReadableStream<Uint8Array>,
    label: string,
    maxOutputBytes?: number
): Promise<Uint8Array> {
    const reader = stream.getReader();
    const chunks: Uint8Array[] = [];
    let totalBytes = 0;
    try {
        for (let next = await reader.read(); !next.done; next = await reader.read()) {
            const value = next.value;
            totalBytes += value.byteLength;
            if (maxOutputBytes !== undefined && totalBytes > maxOutputBytes) {
                await reader.cancel(`decompressed ${label} exceeded ${maxOutputBytes} bytes`);
                throw namedError('CorruptionError', `decompressed ${label} exceeds ${maxOutputBytes} byte limit`);
            }
            chunks.push(value);
        }
    } finally {
        reader.releaseLock();
    }
    if (chunks.length === 1) {
        return chunks[0];
    }
    const out = new Uint8Array(totalBytes);
    let offset = 0;
    for (const chunk of chunks) {
        out.set(chunk, offset);
        offset += chunk.byteLength;
    }
    return out;
}
export function compressionFromStoreFlags(flags: number): CompressionOption {
    const encoded = (flags & STORE_FLAG_COMPRESSION_MASK) >>> STORE_FLAG_COMPRESSION_SHIFT;
    switch (encoded) {
        case 0:
            return false;
        case 1:
            return 'gzip';
        case 2:
            return 'deflate';
        case 3:
            return 'snappy';
        default:
            throw namedError('CorruptionError', `invalid store compression flags: ${encoded}`);
    }
}
export async function encodeStoreValueRecord(value: Uint8Array, compression: CompressionOption): Promise<Uint8Array> {
    validateStoreValueLength(value);
    if (compression === false) {
        return value;
    }
    if (compression === 'snappy') {
        const record = prepareFastStoreValueRecord(value, compression);
        const output = new Uint8Array(record.byteLength);
        writePreparedStoreValueRecord(record, output, 0);
        return output;
    }
    if (value.byteLength < STORE_VALUE_COMPRESSION_THRESHOLD) {
        return buildEnvelope(STORE_RECORD_MAGIC, COMPRESSION_TAG_NONE, value.byteLength, value);
    }
    const stored = new Uint8Array(ENVELOPE_HEADER_SIZE + value.byteLength);
    const payload = stored.subarray(ENVELOPE_HEADER_SIZE);
    payload.set(value);
    const compressed = await collectCompressed(payload, compression, payload.byteLength);
    if (compressed === null || !compressionIsProfitable(payload.byteLength, compressed.totalBytes)) {
        writeEnvelopeHeader(stored, STORE_RECORD_MAGIC, COMPRESSION_TAG_NONE, payload.byteLength, payload);
        return stored;
    }
    return envelopeFromChunks(
        STORE_RECORD_MAGIC,
        compressionTag(compression),
        payload.byteLength,
        compressed.chunks,
        compressed.totalBytes
    );
}
export async function decodeStoreValueRecord(
    value: Uint8Array,
    options: {
        strict: boolean;
    }
): Promise<Uint8Array> {
    const header = readEnvelopeHeader(STORE_RECORD_MAGIC, value, options.strict);
    if (!header) {
        return value;
    }
    if (header.version !== ENVELOPE_VERSION) {
        if (options.strict) {
            throw namedError('CorruptionError', `unsupported value record version: ${header.version}`);
        }
        return value;
    }
    let compression: CompressionOption;
    try {
        compression = compressionKindFromTag(header.kindTag);
    } catch (error) {
        if (options.strict) {
            throw error;
        }
        return value;
    }
    const payload = value.subarray(ENVELOPE_HEADER_SIZE);
    if (crc32(payload) !== header.payloadChecksum) {
        if (options.strict) {
            throw namedError('CorruptionError', 'value record checksum mismatch');
        }
        return value;
    }
    if (compression === false) {
        if (payload.byteLength !== header.rawLength) {
            if (options.strict) {
                throw namedError(
                    'CorruptionError',
                    `value record length mismatch: expected ${header.rawLength} bytes, got ${payload.byteLength}`
                );
            }
            return value;
        }
        return payload.slice();
    }
    if (header.rawLength > MAX_DECOMPRESSED_STORE_VALUE_BYTES) {
        throw namedError(
            'CorruptionError',
            `compressed value advertises ${header.rawLength} decompressed bytes, exceeding the ${MAX_DECOMPRESSED_STORE_VALUE_BYTES} byte limit`
        );
    }
    const decompressed = await decompressBytes(payload, compression, `value record (${compression})`, header.rawLength);
    if (decompressed.byteLength !== header.rawLength) {
        throw namedError(
            'CorruptionError',
            `decompressed value length mismatch: expected ${header.rawLength} bytes, got ${decompressed.byteLength}`
        );
    }
    return decompressed;
}
export async function wrapSnapshotWithCompression(
    snapshot: Uint8Array,
    compression: SnapshotCompressionKind | false
): Promise<Uint8Array> {
    if (compression === false) {
        return snapshot;
    }
    if (!['gzip', 'deflate'].includes(compression)) {
        throw namedError('InternalError', `unsupported snapshot compression kind: ${String(compression)}`);
    }
    if (snapshot.byteLength > MAX_DECOMPRESSED_SNAPSHOT_BYTES) {
        throw namedError(
            'SerializationError',
            `snapshot has ${snapshot.byteLength} bytes, exceeding the ${MAX_DECOMPRESSED_SNAPSHOT_BYTES} byte limit`
        );
    }
    const input = copyBytes(snapshot);
    const compressed = await collectCompressed(input, compression);
    if (compressed === null) {
        throw namedError('InternalError', `failed to compress bytes with ${compression}`);
    }
    return envelopeFromChunks(
        SNAPSHOT_EXPORT_MAGIC,
        compressionTag(compression),
        input.byteLength,
        compressed.chunks,
        compressed.totalBytes
    );
}
export async function unwrapSnapshotCompression(snapshot: Uint8Array): Promise<Uint8Array> {
    const header = readEnvelopeHeader(SNAPSHOT_EXPORT_MAGIC, snapshot);
    if (!header) {
        return snapshot;
    }
    if (header.version !== ENVELOPE_VERSION) {
        throw namedError('CorruptionError', `unsupported snapshot compression version: ${header.version}`);
    }
    const compression = compressionKindFromTag(header.kindTag);
    if (compression === false) {
        throw namedError('CorruptionError', 'snapshot compression envelope cannot store kind "none"');
    }
    if (compression === 'snappy') {
        throw namedError('CorruptionError', `invalid snapshot compression tag: ${header.kindTag}`);
    }
    const payload = snapshot.subarray(ENVELOPE_HEADER_SIZE);
    if (crc32(payload) !== header.payloadChecksum) {
        throw namedError('CorruptionError', 'snapshot compression checksum mismatch');
    }
    if (header.rawLength > MAX_DECOMPRESSED_SNAPSHOT_BYTES) {
        throw namedError(
            'CorruptionError',
            `compressed snapshot advertises ${header.rawLength} decompressed bytes, exceeding the ${MAX_DECOMPRESSED_SNAPSHOT_BYTES} byte limit`
        );
    }
    const decompressed = await decompressBytes(
        payload,
        compression,
        `snapshot export (${compression})`,
        header.rawLength
    );
    if (decompressed.byteLength !== header.rawLength) {
        throw namedError(
            'CorruptionError',
            `decompressed snapshot length mismatch: expected ${header.rawLength} bytes, got ${decompressed.byteLength}`
        );
    }
    return decompressed;
}
