import type { Range } from './types';
const encoder = new TextEncoder();
const decoder = new TextDecoder();
const jsonDecoder = new TextDecoder('utf-8', { fatal: true });
const TYPE_NULL = 0x10;
const TYPE_FALSE = 0x20;
const TYPE_TRUE = 0x21;
const TYPE_NUMBER = 0x30;
const TYPE_STRING = 0x40;
const TYPE_BYTES = 0x50;
const COMPOUND_ESCAPE = 0x00;
const COMPOUND_ESCAPE_CONT = 0xff;
const COMPOUND_TERM = 0x00;
const F64_MASK = BigInt('0xffffffffffffffff');
const sortableNumberBytes = new Uint8Array(9);
const sortableNumberView = new DataView(sortableNumberBytes.buffer);
export type CompoundKeyPart = string | number | boolean | null | Uint8Array;
export type IndexKeyPrimitive = CompoundKeyPart;
export function utf8Encode(value: string): Uint8Array {
    return encoder.encode(value);
}
export function utf8Decode(bytes: Uint8Array): string {
    return decoder.decode(bytes);
}
export function jsonEncode(value: unknown): Uint8Array {
    const serialized = JSON.stringify(value) as string | undefined;
    if (serialized === undefined) {
        throw new TypeError('value has no JSON representation');
    }
    return utf8Encode(serialized);
}
export function jsonDecode<T>(bytes: Uint8Array): T {
    return JSON.parse(jsonDecoder.decode(bytes)) as T;
}
export function u64Key(value: bigint | number): Uint8Array {
    if (typeof value === 'number' && (!Number.isSafeInteger(value) || value < 0)) {
        throw new RangeError('u64Key number inputs must be safe non-negative integers; use bigint for larger values');
    }
    const asBigInt = typeof value === 'bigint' ? value : BigInt(value);
    if (asBigInt < 0n) {
        throw new RangeError('u64Key only accepts non-negative values');
    }
    if (asBigInt > F64_MASK) {
        throw new RangeError('u64Key value exceeds the unsigned 64-bit range');
    }
    const buf = new ArrayBuffer(8);
    const view = new DataView(buf);
    view.setBigUint64(0, asBigInt, false);
    return new Uint8Array(buf);
}
function isCompoundKeyPartList(
    value: CompoundKeyPart | ReadonlyArray<CompoundKeyPart>
): value is ReadonlyArray<CompoundKeyPart> {
    return Array.isArray(value);
}
export function indexKey(value: CompoundKeyPart | ReadonlyArray<CompoundKeyPart>): Uint8Array {
    if (isCompoundKeyPartList(value)) {
        return encodeCompoundKeyList(value);
    }
    return encodeCompoundKeyPart(value);
}
export function compoundKey(...parts: CompoundKeyPart[]): Uint8Array {
    return encodeCompoundKeyList(parts);
}
export function encodeIndexScalar(value: IndexKeyPrimitive): Uint8Array {
    return encodeCompoundKeyPart(value);
}
export function encodeCompoundKeyParts(parts: ReadonlyArray<Uint8Array>): Uint8Array {
    return encodeCompoundBytes(parts);
}
export function splitCompoundKey(key: Uint8Array): Uint8Array[] {
    const parts: Uint8Array[] = [];
    let index = 0;
    while (index < key.length) {
        const start = index;
        let length = 0;
        let closed = false;
        for (; index < key.length; index += 1) {
            const byte = key[index];
            if (byte !== COMPOUND_ESCAPE) {
                length += 1;
                continue;
            }
            const next = key[index + 1];
            if (next === COMPOUND_ESCAPE_CONT) {
                length += 1;
                index += 1;
                continue;
            }
            if (next === COMPOUND_TERM) {
                index += 2;
                closed = true;
                break;
            }
            throw new TypeError('invalid compound key encoding');
        }
        if (!closed) {
            throw new TypeError('compound key terminated unexpectedly');
        }
        const part = new Uint8Array(length);
        let offset = 0;
        for (let cursor = start; cursor < index - 2; cursor += 1) {
            const byte = key[cursor];
            if (byte !== COMPOUND_ESCAPE) {
                part[offset] = byte;
                offset += 1;
                continue;
            }
            part[offset] = COMPOUND_ESCAPE;
            offset += 1;
            cursor += 1;
        }
        parts.push(part);
    }
    return parts;
}
export function prefixSuccessor(prefix: Uint8Array): Uint8Array | null {
    let end = prefix.length;
    while (end > 0 && prefix[end - 1] === 0xff) {
        end -= 1;
    }
    if (end === 0) {
        return null;
    }
    const out = new Uint8Array(end);
    out.set(end === prefix.length ? prefix : prefix.subarray(0, end));
    out[end - 1] += 1;
    return out;
}
export function prefixRange(prefix: Uint8Array): Range {
    const upper = prefixSuccessor(prefix);
    return upper ? { gte: prefix, lt: upper } : { gte: prefix };
}
export function compoundKeyRange(...parts: CompoundKeyPart[]): Range {
    return prefixRange(compoundKey(...parts));
}
function encodeCompoundKeyPart(value: CompoundKeyPart): Uint8Array {
    if (value === null) {
        return Uint8Array.of(TYPE_NULL);
    }
    if (typeof value === 'boolean') {
        return Uint8Array.of(value ? TYPE_TRUE : TYPE_FALSE);
    }
    if (typeof value === 'number') {
        return encodeSortableNumber(value);
    }
    if (typeof value === 'string') {
        return encodeStringKey(value);
    }
    if (value instanceof Uint8Array) {
        const out = new Uint8Array(1 + value.length);
        out[0] = TYPE_BYTES;
        out.set(value, 1);
        return out;
    }
    throw new TypeError('compound key parts must be string, number, boolean, null, or Uint8Array');
}
function encodeCompoundKeyList(parts: ReadonlyArray<CompoundKeyPart>): Uint8Array {
    let length = 0;
    for (let index = 0; index < parts.length; index += 1) {
        length += compoundPartEncodedLength(parts[index]);
    }
    const out = new Uint8Array(length);
    let offset = 0;
    for (let index = 0; index < parts.length; index += 1) {
        offset = writeCompoundPart(out, offset, parts[index]);
    }
    return out;
}
function compoundPartEncodedLength(value: CompoundKeyPart): number {
    if (value === null || typeof value === 'boolean') {
        return 3;
    }
    if (typeof value === 'number') {
        writeSortableNumber(sortableNumberView, value);
        return escapedByteLength(sortableNumberBytes) + 2;
    }
    if (typeof value === 'string') {
        return 1 + walkUtf8(value, null, 0, true) + 2;
    }
    if (value instanceof Uint8Array) {
        return 1 + escapedByteLength(value) + 2;
    }
    throw new TypeError('compound key parts must be string, number, boolean, null, or Uint8Array');
}
function writeCompoundPart(out: Uint8Array, offset: number, value: CompoundKeyPart): number {
    if (value === null) {
        out[offset] = TYPE_NULL;
        return writeTerminator(out, offset + 1);
    }
    if (typeof value === 'boolean') {
        out[offset] = value ? TYPE_TRUE : TYPE_FALSE;
        return writeTerminator(out, offset + 1);
    }
    if (typeof value === 'number') {
        writeSortableNumber(sortableNumberView, value);
        return writeTerminator(out, writeEscapedBytes(out, offset, sortableNumberBytes));
    }
    if (typeof value === 'string') {
        out[offset] = TYPE_STRING;
        return writeTerminator(out, walkUtf8(value, out, offset + 1, true));
    }
    if (value instanceof Uint8Array) {
        out[offset] = TYPE_BYTES;
        return writeTerminator(out, writeEscapedBytes(out, offset + 1, value));
    }
    throw new TypeError('compound key parts must be string, number, boolean, null, or Uint8Array');
}
function encodeCompoundBytes(parts: ReadonlyArray<Uint8Array>): Uint8Array {
    let length = 0;
    for (let index = 0; index < parts.length; index += 1) {
        length += escapedByteLength(parts[index]) + 2;
    }
    const out = new Uint8Array(length);
    let offset = 0;
    for (let index = 0; index < parts.length; index += 1) {
        offset = writeTerminator(out, writeEscapedBytes(out, offset, parts[index]));
    }
    return out;
}
function escapedByteLength(bytes: Uint8Array): number {
    let extra = 0;
    for (let index = 0; index < bytes.length; index += 1) {
        if (bytes[index] === COMPOUND_ESCAPE) {
            extra += 1;
        }
    }
    return bytes.length + extra;
}
function writeEscapedBytes(out: Uint8Array, offset: number, bytes: Uint8Array): number {
    for (let index = 0; index < bytes.length; index += 1) {
        const byte = bytes[index];
        if (byte === COMPOUND_ESCAPE) {
            out[offset] = COMPOUND_ESCAPE;
            out[offset + 1] = COMPOUND_ESCAPE_CONT;
            offset += 2;
        } else {
            out[offset] = byte;
            offset += 1;
        }
    }
    return offset;
}
function writeTerminator(out: Uint8Array, offset: number): number {
    out[offset] = COMPOUND_TERM;
    out[offset + 1] = COMPOUND_TERM;
    return offset + 2;
}
function encodeStringKey(value: string): Uint8Array {
    const length = walkUtf8(value, null, 0, false);
    const out = new Uint8Array(1 + length);
    out[0] = TYPE_STRING;
    walkUtf8(value, out, 1, false);
    return out;
}
// Reject lone surrogates so distinct keys cannot collapse to U+FFFD.
function walkUtf8(value: string, out: Uint8Array | null, offset: number, escapeNul: boolean): number {
    for (let index = 0; index < value.length; index += 1) {
        const code = value.charCodeAt(index);
        if (code <= 0x7f) {
            if (escapeNul && code === 0) {
                if (out !== null) {
                    out[offset] = COMPOUND_ESCAPE;
                    out[offset + 1] = COMPOUND_ESCAPE_CONT;
                }
                offset += 2;
            } else {
                if (out !== null) {
                    out[offset] = code;
                }
                offset += 1;
            }
            continue;
        }
        if (code <= 0x7ff) {
            if (out !== null) {
                out[offset] = 0xc0 | (code >> 6);
                out[offset + 1] = 0x80 | (code & 0x3f);
            }
            offset += 2;
            continue;
        }
        if (code >= 0xd800 && code <= 0xdbff) {
            const next = value.charCodeAt(index + 1);
            if (next >= 0xdc00 && next <= 0xdfff) {
                const point = ((code - 0xd800) << 10) + (next - 0xdc00) + 0x10000;
                if (out !== null) {
                    out[offset] = 0xf0 | (point >> 18);
                    out[offset + 1] = 0x80 | ((point >> 12) & 0x3f);
                    out[offset + 2] = 0x80 | ((point >> 6) & 0x3f);
                    out[offset + 3] = 0x80 | (point & 0x3f);
                }
                offset += 4;
                index += 1;
                continue;
            }
        }
        if (code >= 0xd800 && code <= 0xdfff) {
            throw new TypeError('compound key string parts must contain valid Unicode scalar values');
        }
        if (out !== null) {
            out[offset] = 0xe0 | (code >> 12);
            out[offset + 1] = 0x80 | ((code >> 6) & 0x3f);
            out[offset + 2] = 0x80 | (code & 0x3f);
        }
        offset += 3;
    }
    return offset;
}
function encodeSortableNumber(value: number): Uint8Array {
    const bytes = new Uint8Array(9);
    writeSortableNumber(new DataView(bytes.buffer), value);
    return bytes;
}
function writeSortableNumber(view: DataView, value: number): void {
    if (!Number.isFinite(value)) {
        throw new TypeError('compound key number parts must be finite');
    }
    const normalized = Object.is(value, -0) ? 0 : value;
    view.setUint8(0, TYPE_NUMBER);
    view.setFloat64(1, normalized, false);
    let bits = view.getBigUint64(1, false);
    bits = (bits & (1n << 63n)) !== 0n ? ~bits & F64_MASK : bits ^ (1n << 63n);
    view.setBigUint64(1, bits, false);
}
