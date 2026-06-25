import { utf8Encode } from './codec';
import type { CompressionKind } from './types';
export type CompressionOption = CompressionKind | false;
const STORE_RECORD_MAGIC = utf8Encode('BDBZVAL1');
const SNAPSHOT_EXPORT_MAGIC = utf8Encode('BDBZSNP1');
const ENVELOPE_VERSION = 1;
const ENVELOPE_HEADER_SIZE = 18;
const COMPRESSION_TAG_NONE = 0;
const COMPRESSION_TAG_GZIP = 1;
const COMPRESSION_TAG_DEFLATE = 2;
const STORE_FLAG_COMPRESSION_SHIFT = 2;
const STORE_FLAG_COMPRESSION_MASK = 0b11 << STORE_FLAG_COMPRESSION_SHIFT;
export const STORE_VALUE_COMPRESSION_THRESHOLD = 1024;
const CRC32_TABLE = (() => {
    const table = new Uint32Array(256);
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
    return table;
})();
type EnvelopeHeader = {
    version: number;
    kindTag: number;
    rawLength: number;
    payloadChecksum: number;
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
    for (const byte of bytes) {
        crc = (crc >>> 8) ^ CRC32_TABLE[(crc ^ byte) & 0xff]!;
    }
    return (crc ^ 0xffffffff) >>> 0;
}
function buildEnvelope(magic: Uint8Array, kindTag: number, rawLength: number, payload: Uint8Array): Uint8Array {
    const out = new Uint8Array(ENVELOPE_HEADER_SIZE + payload.byteLength);
    out.set(magic, 0);
    out[8] = ENVELOPE_VERSION;
    out[9] = kindTag;
    const view = new DataView(out.buffer, out.byteOffset, ENVELOPE_HEADER_SIZE);
    view.setUint32(10, rawLength, true);
    view.setUint32(14, crc32(payload), true);
    out.set(payload, ENVELOPE_HEADER_SIZE);
    return out;
}
function toOwnedArrayBuffer(bytes: Uint8Array): ArrayBuffer {
    const copy = new Uint8Array(bytes.byteLength);
    copy.set(bytes);
    return copy.buffer;
}
function readEnvelopeHeader(magic: Uint8Array, bytes: Uint8Array): EnvelopeHeader | null {
    if (bytes.byteLength < ENVELOPE_HEADER_SIZE) {
        return null;
    }
    for (let index = 0; index < magic.byteLength; index += 1) {
        if (bytes[index] !== magic[index]) {
            return null;
        }
    }
    const view = new DataView(bytes.buffer, bytes.byteOffset, ENVELOPE_HEADER_SIZE);
    return {
        version: bytes[8]!,
        kindTag: bytes[9]!,
        rawLength: view.getUint32(10, true),
        payloadChecksum: view.getUint32(14, true)
    };
}
async function compressBytes(data: Uint8Array, kind: CompressionKind): Promise<Uint8Array> {
    ensureCompressionRuntime();
    let stream: ReadableStream<Uint8Array>;
    try {
        stream = new Blob([toOwnedArrayBuffer(data)]).stream().pipeThrough(new CompressionStream(kind));
    } catch {
        throw compressionRuntimeUnavailable();
    }
    try {
        return new Uint8Array(await new Response(stream).arrayBuffer());
    } catch (error) {
        throw namedError('InternalError', `failed to compress bytes with ${kind}: ${String(error)}`);
    }
}
async function decompressBytes(data: Uint8Array, kind: CompressionKind, label: string): Promise<Uint8Array> {
    ensureCompressionRuntime();
    let stream: ReadableStream<Uint8Array>;
    try {
        stream = new Blob([toOwnedArrayBuffer(data)]).stream().pipeThrough(new DecompressionStream(kind));
    } catch {
        throw compressionRuntimeUnavailable();
    }
    try {
        return new Uint8Array(await new Response(stream).arrayBuffer());
    } catch (error) {
        throw namedError('CorruptionError', `failed to decompress ${label}: ${String(error)}`);
    }
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
        default:
            throw namedError('CorruptionError', `invalid store compression flags: ${encoded}`);
    }
}
export async function encodeStoreValueRecord(value: Uint8Array, compression: CompressionOption): Promise<Uint8Array> {
    if (compression === false) {
        return value;
    }
    if (value.byteLength < STORE_VALUE_COMPRESSION_THRESHOLD) {
        return buildEnvelope(STORE_RECORD_MAGIC, COMPRESSION_TAG_NONE, value.byteLength, value);
    }
    const compressed = await compressBytes(value, compression);
    if (compressed.byteLength >= value.byteLength) {
        return buildEnvelope(STORE_RECORD_MAGIC, COMPRESSION_TAG_NONE, value.byteLength, value);
    }
    return buildEnvelope(STORE_RECORD_MAGIC, compressionTag(compression), value.byteLength, compressed);
}
export async function decodeStoreValueRecord(
    value: Uint8Array,
    options: {
        strict: boolean;
    }
): Promise<Uint8Array> {
    const header = readEnvelopeHeader(STORE_RECORD_MAGIC, value);
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
    const decompressed = await decompressBytes(payload, compression, `value record (${compression})`);
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
    compression: CompressionOption
): Promise<Uint8Array> {
    if (compression === false) {
        return snapshot;
    }
    const compressed = await compressBytes(snapshot, compression);
    return buildEnvelope(SNAPSHOT_EXPORT_MAGIC, compressionTag(compression), snapshot.byteLength, compressed);
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
    const payload = snapshot.subarray(ENVELOPE_HEADER_SIZE);
    if (crc32(payload) !== header.payloadChecksum) {
        throw namedError('CorruptionError', 'snapshot compression checksum mismatch');
    }
    const decompressed = await decompressBytes(payload, compression, `snapshot export (${compression})`);
    if (decompressed.byteLength !== header.rawLength) {
        throw namedError(
            'CorruptionError',
            `decompressed snapshot length mismatch: expected ${header.rawLength} bytes, got ${decompressed.byteLength}`
        );
    }
    return decompressed;
}
