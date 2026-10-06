import { InvalidOpenOptionsError, InvalidRangeError, ReservedStoreNameError, SerializationError } from './errors';
import { compareStringsByCodeUnit } from './internal';
import { encodeCompoundKeyParts, encodeIndexScalar, utf8Encode } from './codec';
import type { IndexDef, Range } from './types';
const fatalDecoder = new TextDecoder('utf-8', { fatal: true });
const MISSING = Symbol('moyodb.indexing.missing');
const FNV64_OFFSET = BigInt('0xcbf29ce484222325');
const FNV64_PRIME = BigInt('0x100000001b3');
const FNV64_MASK = BigInt('0xffffffffffffffff');
// Persisted store names use this legacy namespace. Renaming requires a migration.
const INTERNAL_STORE_PREFIX = '__browserdb:';
const INTERNAL_INDEX_STORE_PREFIX = '__browserdb:index:';
export const INDEX_METADATA_STORE = '__browserdb:indexes';
export interface NormalizedIndexDef {
    store: string;
    name: string;
    keyPath: string[];
    compound: boolean;
    unique: boolean;
    internalStore: string;
}
export interface DecodedIndexEntryKey {
    logicalKey: Uint8Array;
    primaryKey: Uint8Array;
}
export function isInternalStoreName(name: string): boolean {
    return name.startsWith(INTERNAL_STORE_PREFIX);
}
export function assertPublicStoreName(name: string): void {
    if (isInternalStoreName(name)) {
        throw new ReservedStoreNameError(`reserved store name: ${name}`);
    }
}
export function normalizeIndexDefinitions(value: unknown): NormalizedIndexDef[] {
    if (value === undefined) {
        return [];
    }
    if (!Array.isArray(value)) {
        throw new InvalidOpenOptionsError('indexes must be an array');
    }
    const normalized: NormalizedIndexDef[] = [];
    const identities = new Set<string>();
    const internalStores = new Map<string, string>();
    for (const entry of value) {
        if (entry === null || typeof entry !== 'object' || Array.isArray(entry)) {
            throw new InvalidOpenOptionsError('each index definition must be an object');
        }
        const { store, name, keyPath, unique } = entry as {
            store?: unknown;
            name?: unknown;
            keyPath?: unknown;
            unique?: unknown;
        };
        if (typeof store !== 'string' || store.length === 0) {
            throw new InvalidOpenOptionsError('index store must be a non-empty string');
        }
        if (typeof name !== 'string' || name.length === 0) {
            throw new InvalidOpenOptionsError('index name must be a non-empty string');
        }
        assertPublicStoreName(store);
        const keyPathInfo = normalizeIndexKeyPath(keyPath);
        if (unique !== undefined && typeof unique !== 'boolean') {
            throw new InvalidOpenOptionsError('index unique must be a boolean');
        }
        const def: NormalizedIndexDef = {
            store,
            name,
            keyPath: keyPathInfo.paths,
            compound: keyPathInfo.compound,
            unique: unique ?? false,
            internalStore: makeInternalIndexStoreName(store, name)
        };
        const identity = indexDefinitionIdentity(def.store, def.name);
        if (identities.has(identity)) {
            throw new InvalidOpenOptionsError(`duplicate index definition ${def.store}.${def.name}`);
        }
        identities.add(identity);
        const mappedIdentity = internalStores.get(def.internalStore);
        if (mappedIdentity && mappedIdentity !== identity) {
            throw new InvalidOpenOptionsError(
                `index definitions ${mappedIdentity} and ${identity} map to the same internal store ${def.internalStore}`
            );
        }
        internalStores.set(def.internalStore, identity);
        normalized.push(def);
    }
    normalized.sort(compareNormalizedIndexDefinitions);
    return normalized;
}
export function compareNormalizedIndexDefinitions(left: NormalizedIndexDef, right: NormalizedIndexDef): number {
    const storeCompare = compareStringsByCodeUnit(left.store, right.store);
    if (storeCompare !== 0) {
        return storeCompare;
    }
    const nameCompare = compareStringsByCodeUnit(left.name, right.name);
    if (nameCompare !== 0) {
        return nameCompare;
    }
    const uniqueCompare = Number(left.unique) - Number(right.unique);
    if (uniqueCompare !== 0) {
        return uniqueCompare;
    }
    return compareStringsByCodeUnit(serializeIndexKeyPath(left), serializeIndexKeyPath(right));
}
export function indexDefinitionIdentity(store: string, name: string): string {
    return JSON.stringify([store, name]);
}
export function findIndexDefinition(
    defs: ReadonlyArray<NormalizedIndexDef>,
    store: string,
    name: string
): NormalizedIndexDef | null {
    return defs.find((def) => def.store === store && def.name === name) ?? null;
}
export function indexesForStore(defs: ReadonlyArray<NormalizedIndexDef>, store: string): NormalizedIndexDef[] {
    return defs.filter((def) => def.store === store);
}
export function encodeIndexMetadataKey(store: string, name: string): Uint8Array {
    return encodeCompoundKeyParts([utf8Encode(store), utf8Encode(name)]);
}
export function encodeIndexMetadataValue(def: NormalizedIndexDef): Uint8Array {
    return utf8Encode(JSON.stringify(toPublicIndexDefinition(def)));
}
export function decodeIndexMetadataValue(bytes: Uint8Array): NormalizedIndexDef {
    let parsed: unknown;
    try {
        parsed = JSON.parse(fatalDecoder.decode(bytes));
        const [def] = normalizeIndexDefinitions([parsed]);
        return def;
    } catch (error) {
        throw new SerializationError(`invalid persisted index metadata: ${String(error)}`);
    }
}
export function encodeIndexEntryKey(logicalIndexKey: Uint8Array, primaryKey: Uint8Array): Uint8Array {
    const out = new Uint8Array(compoundEncodedLength(logicalIndexKey) + compoundEncodedLength(primaryKey));
    const primaryAt = writeCompoundPart(out, 0, logicalIndexKey);
    writeCompoundPart(out, primaryAt, primaryKey);
    return out;
}
export function decodeIndexEntryKey(bytes: Uint8Array): DecodedIndexEntryKey {
    try {
        return decodeTwoPartIndexKey(bytes);
    } catch (error) {
        if (error instanceof SerializationError) {
            throw error;
        }
        throw new SerializationError(`invalid index entry key encoding: ${String(error)}`);
    }
}
export function indexKeyExactRange(logicalIndexKey: Uint8Array): Range {
    const gte = encodeCompoundPrefix(logicalIndexKey);
    const lt = advanceEncodedPrefix(gte.slice());
    return lt ? { gte, lt } : { gte };
}
/** WASM `usize` is 32-bit, so this is the largest limit the engine can accept. */
export const MAX_ENGINE_SCAN_LIMIT = 0xffff_ffff;

/**
 * Bounds and limits follow the engine `RangeSpec` contract.
 * Conflicting bounds and a limit the engine cannot store are `InvalidRangeError`.
 */
export function assertValidScanRange(range: Range = {}): void {
    if (range.gt !== undefined && range.gte !== undefined) {
        throw new InvalidRangeError('range cannot include both gt and gte');
    }
    if (range.lt !== undefined && range.lte !== undefined) {
        throw new InvalidRangeError('range cannot include both lt and lte');
    }
    if (
        range.limit !== undefined &&
        (!Number.isSafeInteger(range.limit) || range.limit < 0 || range.limit > MAX_ENGINE_SCAN_LIMIT)
    ) {
        throw new InvalidRangeError('limit must be an integer between 0 and 4294967295');
    }
}

export function indexRangeToPhysicalRange(range: Range = {}): Range {
    assertValidScanRange(range);
    const physical: Range = {};
    if (range.reverse !== undefined) {
        physical.reverse = range.reverse;
    }
    if (range.limit !== undefined) {
        physical.limit = range.limit;
    }
    if (range.gte) {
        physical.gte = encodeCompoundPrefix(range.gte);
    }
    if (range.gt) {
        const lower = advanceEncodedPrefix(encodeCompoundPrefix(range.gt));
        if (lower) {
            physical.gte = lower;
        }
    }
    if (range.lt) {
        physical.lt = encodeCompoundPrefix(range.lt);
    }
    if (range.lte) {
        const upper = advanceEncodedPrefix(encodeCompoundPrefix(range.lte));
        if (upper) {
            physical.lt = upper;
        }
    }
    return physical;
}
export function extractLogicalIndexKey(def: NormalizedIndexDef, valueBytes: Uint8Array): Uint8Array | null {
    return extractLogicalIndexKeyFromDocument(def, decodeIndexedDocument(valueBytes));
}
/**
 * Reuse the decoded document only within one synchronous mutation plan.
 * Lazy decoding preserves validation order. Do not retain the extractor
 * across operations or an await.
 */
export function createIndexKeyExtractor(valueBytes: Uint8Array): (def: NormalizedIndexDef) => Uint8Array | null {
    let decoded = false;
    let documentValue: unknown;
    return (def) => {
        if (!decoded) {
            documentValue = decodeIndexedDocument(valueBytes);
            decoded = true;
        }
        return extractLogicalIndexKeyFromDocument(def, documentValue);
    };
}
export function extractLogicalIndexKeyFromDocument(def: NormalizedIndexDef, documentValue: unknown): Uint8Array | null {
    if (def.compound) {
        const parts: Uint8Array[] = [];
        for (const keyPath of def.keyPath) {
            const resolved = resolveKeyPath(documentValue, keyPath);
            if (resolved === MISSING) {
                return null;
            }
            parts.push(encodeDocumentIndexValue(resolved));
        }
        return encodeCompoundKeyParts(parts);
    }
    const resolved = resolveKeyPath(documentValue, def.keyPath[0]);
    if (resolved === MISSING) {
        return null;
    }
    return encodeDocumentIndexValue(resolved);
}
export function serializeNormalizedIndexDefinitions(defs: ReadonlyArray<NormalizedIndexDef>): string {
    return JSON.stringify(toPublicIndexDefinitions(defs));
}
export function cloneNormalizedIndexDefinitions(defs: ReadonlyArray<NormalizedIndexDef>): NormalizedIndexDef[] {
    return defs.map((def) => ({
        store: def.store,
        name: def.name,
        keyPath: [...def.keyPath],
        compound: def.compound,
        unique: def.unique,
        internalStore: def.internalStore
    }));
}
export function toPublicIndexDefinitions(defs: ReadonlyArray<NormalizedIndexDef>): IndexDef[] {
    return defs.map(toPublicIndexDefinition);
}
function toPublicIndexDefinition(def: NormalizedIndexDef): IndexDef {
    return {
        store: def.store,
        name: def.name,
        keyPath: def.compound ? [...def.keyPath] : def.keyPath[0],
        unique: def.unique
    };
}
function normalizeIndexKeyPath(value: unknown): {
    paths: string[];
    compound: boolean;
} {
    if (typeof value === 'string') {
        validateSingleKeyPath(value);
        return { paths: [value], compound: false };
    }
    if (
        !Array.isArray(value) ||
        value.length === 0 ||
        !value.every((item): item is string => typeof item === 'string')
    ) {
        throw new InvalidOpenOptionsError('index keyPath must be a non-empty string or non-empty string[]');
    }
    for (const path of value) {
        validateSingleKeyPath(path);
    }
    return { paths: [...value], compound: true };
}
function validateSingleKeyPath(value: string): void {
    if (value.length === 0) {
        throw new InvalidOpenOptionsError('index keyPath strings must be non-empty');
    }
    if (value.startsWith('.') || value.endsWith('.') || value.includes('..')) {
        throw new InvalidOpenOptionsError(`invalid index keyPath: ${value}`);
    }
}
function serializeIndexKeyPath(def: NormalizedIndexDef): string {
    return def.compound ? `[${def.keyPath.join(',')}]` : def.keyPath[0];
}
function makeInternalIndexStoreName(store: string, name: string): string {
    const storeToken = base64Url(utf8Encode(store));
    const nameToken = base64Url(utf8Encode(name));
    const candidate = `${INTERNAL_INDEX_STORE_PREFIX}${storeToken}:${nameToken}`;
    if (utf8Encode(candidate).length <= 255) {
        return candidate;
    }
    // Preserve the persisted hash format. Normalization rejects distinct
    // identities that map to the same internal store, including separator collisions.
    const payload = utf8Encode(`${store}\u0000${name}`);
    return `${INTERNAL_INDEX_STORE_PREFIX}h:${fnv1a64Hex(payload)}`;
}
function base64Url(bytes: Uint8Array): string {
    let binary = '';
    for (const byte of bytes) {
        binary += String.fromCharCode(byte);
    }
    return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/u, '');
}
function fnv1a64Hex(bytes: Uint8Array): string {
    let hash = FNV64_OFFSET;
    for (const byte of bytes) {
        hash ^= BigInt(byte);
        hash = (hash * FNV64_PRIME) & FNV64_MASK;
    }
    return hash.toString(16).padStart(16, '0');
}
export function decodeIndexedDocument(bytes: Uint8Array): unknown {
    try {
        return JSON.parse(fatalDecoder.decode(bytes));
    } catch (error) {
        throw new SerializationError(`indexed values must be valid UTF-8 JSON: ${String(error)}`);
    }
}
function resolveKeyPath(root: unknown, keyPath: string): unknown {
    let current: unknown = root;
    for (const segment of keyPath.split('.')) {
        // JSON strings and numbers have no nested fields. Boxing them would index
        // UTF-16 code units and string length, which JSON.stringify does not preserve.
        if (current === null || typeof current !== 'object') {
            return MISSING;
        }
        if (!Object.hasOwn(current, segment)) {
            return MISSING;
        }
        current = Reflect.get(current, segment);
    }
    return current;
}
function encodeDocumentIndexValue(value: unknown): Uint8Array {
    if (
        value === null ||
        typeof value === 'string' ||
        typeof value === 'number' ||
        typeof value === 'boolean' ||
        value instanceof Uint8Array
    ) {
        try {
            return encodeIndexScalar(value);
        } catch (error) {
            if (error instanceof TypeError) {
                throw new SerializationError(`invalid indexed keyPath value: ${error.message}`);
            }
            throw error;
        }
    }
    throw new SerializationError(
        `indexed keyPath values must resolve to string, number, boolean, null, or Uint8Array; got ${describeValue(value)}`
    );
}
function describeValue(value: unknown): string {
    if (Array.isArray(value)) {
        return 'array';
    }
    return typeof value;
}
const INDEX_KEY_ESCAPE = 0x00;
const INDEX_KEY_ESCAPE_CONT = 0xff;
const INDEX_KEY_TERM = 0x00;
function compoundEncodedLength(part: Uint8Array): number {
    let zeros = 0;
    const length = part.length;
    for (let index = 0; index < length; index += 1) {
        if (part[index] === INDEX_KEY_ESCAPE) {
            zeros += 1;
        }
    }
    return length + zeros + 2;
}
function writeCompoundPart(out: Uint8Array, offset: number, part: Uint8Array): number {
    const length = part.length;
    for (let index = 0; index < length; index += 1) {
        const byte = part[index];
        if (byte === INDEX_KEY_ESCAPE) {
            out[offset] = INDEX_KEY_ESCAPE;
            out[offset + 1] = INDEX_KEY_ESCAPE_CONT;
            offset += 2;
        } else {
            out[offset] = byte;
            offset += 1;
        }
    }
    out[offset] = INDEX_KEY_TERM;
    out[offset + 1] = INDEX_KEY_TERM;
    return offset + 2;
}
function encodeCompoundPrefix(part: Uint8Array): Uint8Array {
    const out = new Uint8Array(compoundEncodedLength(part));
    writeCompoundPart(out, 0, part);
    return out;
}
// In-place prefixSuccessor. Compound prefixes end in 0x00, so the final byte
// advances inside this buffer instead of allocating a second copy.
function advanceEncodedPrefix(prefix: Uint8Array): Uint8Array | null {
    for (let index = prefix.length - 1; index >= 0; index -= 1) {
        if (prefix[index] !== 0xff) {
            prefix[index] += 1;
            return index + 1 === prefix.length ? prefix : prefix.subarray(0, index + 1);
        }
    }
    return null;
}
// Count both parts, then fill exact buffers. splitCompoundKey would also build a
// temporary number list for every byte of every scanned index row.
function decodeTwoPartIndexKey(bytes: Uint8Array): DecodedIndexEntryKey {
    const cursor = { unescaped: 0, end: 0 };
    if (!readCompoundPart(bytes, 0, cursor)) {
        throw new SerializationError('invalid index entry key encoding');
    }
    const logicalLength = cursor.unescaped;
    const primaryStart = cursor.end;
    if (!readCompoundPart(bytes, primaryStart, cursor)) {
        throw new SerializationError('invalid index entry key encoding');
    }
    const primaryLength = cursor.unescaped;
    let end = cursor.end;
    if (end !== bytes.length) {
        for (;;) {
            if (!readCompoundPart(bytes, end, cursor)) {
                throw new SerializationError('invalid index entry key encoding');
            }
            if (cursor.end <= end) {
                throw new TypeError('invalid compound key encoding');
            }
            end = cursor.end;
            if (end === bytes.length) {
                throw new SerializationError('invalid index entry key encoding');
            }
        }
    }
    const logicalKey = new Uint8Array(logicalLength);
    const primaryKey = new Uint8Array(primaryLength);
    copyUnescapedPart(bytes, 0, logicalKey);
    copyUnescapedPart(bytes, primaryStart, primaryKey);
    return { logicalKey, primaryKey };
}
function readCompoundPart(bytes: Uint8Array, start: number, cursor: { unescaped: number; end: number }): boolean {
    if (start >= bytes.length) {
        return false;
    }
    let unescaped = 0;
    for (let index = start; index < bytes.length; index += 1) {
        const byte = bytes[index];
        if (byte !== INDEX_KEY_ESCAPE) {
            unescaped += 1;
            continue;
        }
        const next = bytes[index + 1];
        if (next === INDEX_KEY_ESCAPE_CONT) {
            unescaped += 1;
            index += 1;
            continue;
        }
        if (next === INDEX_KEY_TERM) {
            cursor.unescaped = unescaped;
            cursor.end = index + 2;
            return true;
        }
        throw new TypeError('invalid compound key encoding');
    }
    throw new TypeError('compound key terminated unexpectedly');
}
function copyUnescapedPart(bytes: Uint8Array, start: number, out: Uint8Array): void {
    const needed = out.length;
    let offset = 0;
    for (let index = start; offset < needed; index += 1) {
        const byte = bytes[index];
        if (byte !== INDEX_KEY_ESCAPE) {
            out[offset] = byte;
            offset += 1;
            continue;
        }
        if (bytes[index + 1] === INDEX_KEY_ESCAPE_CONT) {
            out[offset] = INDEX_KEY_ESCAPE;
            offset += 1;
            index += 1;
            continue;
        }
        return;
    }
}
