import type { Range } from './types';
const encoder = new TextEncoder();
const decoder = new TextDecoder();
const TYPE_NULL = 0x10;
const TYPE_FALSE = 0x20;
const TYPE_TRUE = 0x21;
const TYPE_NUMBER = 0x30;
const TYPE_STRING = 0x40;
const TYPE_BYTES = 0x50;
const COMPOUND_ESCAPE = 0x00;
const COMPOUND_ESCAPE_CONT = 0xff;
const COMPOUND_TERM = 0x00;
const F64_MASK = 0xffffffffffffffffn;
export type CompoundKeyPart = string | number | boolean | null | Uint8Array;
export type IndexKeyPrimitive = CompoundKeyPart;
export function utf8Encode(value: string): Uint8Array {
    return encoder.encode(value);
}
export function utf8Decode(bytes: Uint8Array): string {
    return decoder.decode(bytes);
}
export function jsonEncode(value: unknown): Uint8Array {
    return utf8Encode(JSON.stringify(value));
}
export function jsonDecode<T>(bytes: Uint8Array): T {
    return JSON.parse(utf8Decode(bytes)) as T;
}
export function u64Key(value: bigint | number): Uint8Array {
    const asBigInt = typeof value === 'bigint' ? value : BigInt(value);
    if (asBigInt < 0n) {
        throw new RangeError('u64Key only accepts non-negative values');
    }
    const buf = new ArrayBuffer(8);
    const view = new DataView(buf);
    view.setBigUint64(0, asBigInt, false);
    return new Uint8Array(buf);
}
export function indexKey(value: CompoundKeyPart | ReadonlyArray<CompoundKeyPart>): Uint8Array {
    if (Array.isArray(value)) {
        return compoundKey(...value);
    }
    return encodeCompoundKeyPart(value as CompoundKeyPart);
}
export function compoundKey(...parts: CompoundKeyPart[]): Uint8Array {
    return encodeCompoundBytes(parts.map((part) => encodeCompoundKeyPart(part)));
}
export function encodeIndexScalar(value: IndexKeyPrimitive): Uint8Array {
    return encodeCompoundKeyPart(value);
}
export function encodeCompoundKeyParts(parts: ReadonlyArray<Uint8Array>): Uint8Array {
    return encodeCompoundBytes(parts);
}
export function splitCompoundKey(key: Uint8Array): Uint8Array[] {
    const parts: Uint8Array[] = [];
    const current: number[] = [];
    for (let index = 0; index < key.length; index += 1) {
        const byte = key[index]!;
        if (byte !== COMPOUND_ESCAPE) {
            current.push(byte);
            continue;
        }
        const next = key[index + 1];
        if (next === COMPOUND_ESCAPE_CONT) {
            current.push(COMPOUND_ESCAPE);
            index += 1;
            continue;
        }
        if (next === COMPOUND_TERM) {
            parts.push(Uint8Array.from(current));
            current.length = 0;
            index += 1;
            continue;
        }
        throw new TypeError('invalid compound key encoding');
    }
    if (current.length !== 0) {
        throw new TypeError('compound key terminated unexpectedly');
    }
    return parts;
}
export function prefixSuccessor(prefix: Uint8Array): Uint8Array | null {
    const out = prefix.slice();
    for (let index = out.length - 1; index >= 0; index -= 1) {
        if (out[index] !== 0xff) {
            out[index] += 1;
            return out.subarray(0, index + 1);
        }
    }
    return null;
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
        const body = utf8Encode(value);
        const out = new Uint8Array(1 + body.length);
        out[0] = TYPE_STRING;
        out.set(body, 1);
        return out;
    }
    if (value instanceof Uint8Array) {
        const out = new Uint8Array(1 + value.length);
        out[0] = TYPE_BYTES;
        out.set(value, 1);
        return out;
    }
    throw new TypeError('compound key parts must be string, number, boolean, null, or Uint8Array');
}
function encodeCompoundBytes(parts: ReadonlyArray<Uint8Array>): Uint8Array {
    let length = 0;
    for (const part of parts) {
        for (const byte of part) {
            length += byte === COMPOUND_ESCAPE ? 2 : 1;
        }
        length += 2;
    }
    const out = new Uint8Array(length);
    let offset = 0;
    for (const part of parts) {
        for (const byte of part) {
            if (byte === COMPOUND_ESCAPE) {
                out[offset] = COMPOUND_ESCAPE;
                out[offset + 1] = COMPOUND_ESCAPE_CONT;
                offset += 2;
            } else {
                out[offset] = byte;
                offset += 1;
            }
        }
        out[offset] = COMPOUND_TERM;
        out[offset + 1] = COMPOUND_TERM;
        offset += 2;
    }
    return out;
}
function encodeSortableNumber(value: number): Uint8Array {
    if (!Number.isFinite(value)) {
        throw new TypeError('compound key number parts must be finite');
    }
    const normalized = Object.is(value, -0) ? 0 : value;
    const buffer = new ArrayBuffer(9);
    const view = new DataView(buffer);
    view.setUint8(0, TYPE_NUMBER);
    view.setFloat64(1, normalized, false);
    let bits = view.getBigUint64(1, false);
    bits = (bits & (1n << 63n)) !== 0n ? ~bits & F64_MASK : bits ^ (1n << 63n);
    view.setBigUint64(1, bits, false);
    return new Uint8Array(buffer);
}
