import { InvalidOpenOptionsError, ReservedStoreNameError, SerializationError } from './errors';
import { compareStringsByCodeUnit } from './internal';
import {
    encodeCompoundKeyParts,
    encodeIndexScalar,
    prefixRange,
    prefixSuccessor,
    splitCompoundKey,
    utf8Encode,
    type IndexKeyPrimitive
} from './codec';
import type { IndexDef, Range } from './types';
const encoder = new TextEncoder();
const fatalDecoder = new TextDecoder('utf-8', { fatal: true });
const MISSING = Symbol('moyodb.indexing.missing');
const FNV64_OFFSET = 0xcbf29ce484222325n;
const FNV64_PRIME = 0x100000001b3n;
const FNV64_MASK = 0xffffffffffffffffn;
// Legacy SDK internal namespace preserved for storage-format compatibility; do not rename without a migration.
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
    return `${store}\u0000${name}`;
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
    } catch (error) {
        throw new SerializationError(`invalid persisted index metadata: ${String(error)}`);
    }
    const [def] = normalizeIndexDefinitions([parsed]);
    return def;
}
export function encodeIndexEntryKey(logicalIndexKey: Uint8Array, primaryKey: Uint8Array): Uint8Array {
    return encodeCompoundKeyParts([logicalIndexKey, primaryKey]);
}
export function decodeIndexEntryKey(bytes: Uint8Array): DecodedIndexEntryKey {
    const parts = splitCompoundKey(bytes);
    if (parts.length !== 2) {
        throw new SerializationError('invalid index entry key encoding');
    }
    return {
        logicalKey: parts[0]!,
        primaryKey: parts[1]!
    };
}
export function indexKeyExactRange(logicalIndexKey: Uint8Array): Range {
    return prefixRange(encodeCompoundKeyParts([logicalIndexKey]));
}
export function indexRangeToPhysicalRange(range: Range = {}): Range {
    validateIndexRangeShape(range);
    const physical: Range = {};
    if (range.reverse !== undefined) {
        physical.reverse = range.reverse;
    }
    if (range.limit !== undefined) {
        physical.limit = range.limit;
    }
    if (range.gte) {
        physical.gte = encodeCompoundKeyParts([range.gte]);
    }
    if (range.gt) {
        const lower = prefixSuccessor(encodeCompoundKeyParts([range.gt]));
        if (lower) {
            physical.gte = lower;
        }
    }
    if (range.lt) {
        physical.lt = encodeCompoundKeyParts([range.lt]);
    }
    if (range.lte) {
        const upper = prefixSuccessor(encodeCompoundKeyParts([range.lte]));
        if (upper) {
            physical.lt = upper;
        }
    }
    return physical;
}
export function extractLogicalIndexKey(def: NormalizedIndexDef, valueBytes: Uint8Array): Uint8Array | null {
    const documentValue = decodeIndexedDocument(valueBytes);
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
    const resolved = resolveKeyPath(documentValue, def.keyPath[0]!);
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
        keyPath: def.compound ? [...def.keyPath] : def.keyPath[0]!,
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
    if (!Array.isArray(value) || value.length === 0 || value.some((item) => typeof item !== 'string')) {
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
    return def.compound ? `[${def.keyPath.join(',')}]` : def.keyPath[0]!;
}
function makeInternalIndexStoreName(store: string, name: string): string {
    const storeToken = base64Url(utf8Encode(store));
    const nameToken = base64Url(utf8Encode(name));
    const candidate = `${INTERNAL_INDEX_STORE_PREFIX}${storeToken}:${nameToken}`;
    if (encoder.encode(candidate).length <= 255) {
        return candidate;
    }
    const payload = utf8Encode(indexDefinitionIdentity(store, name));
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
function decodeIndexedDocument(bytes: Uint8Array): unknown {
    try {
        return JSON.parse(fatalDecoder.decode(bytes));
    } catch (error) {
        throw new SerializationError(`indexed values must be valid UTF-8 JSON: ${String(error)}`);
    }
}
function resolveKeyPath(root: unknown, keyPath: string): unknown | typeof MISSING {
    let current: unknown = root;
    for (const segment of keyPath.split('.')) {
        if (current === null || current === undefined) {
            return MISSING;
        }
        const target = Object(current) as Record<string, unknown>;
        if (!(segment in target)) {
            return MISSING;
        }
        current = Reflect.get(target, segment);
    }
    return current;
}
function encodeDocumentIndexValue(value: unknown): Uint8Array {
    if (value instanceof Uint8Array) {
        return encodeIndexScalar(value);
    }
    if (value === null || typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
        return encodeIndexScalar(value as IndexKeyPrimitive);
    }
    throw new SerializationError(
        `indexed keyPath values must resolve to string, number, boolean, null, or Uint8Array; got ${describeValue(value)}`
    );
}
function describeValue(value: unknown): string {
    if (Array.isArray(value)) {
        return 'array';
    }
    if (value === null) {
        return 'null';
    }
    return typeof value;
}
function validateIndexRangeShape(range: Range): void {
    if (range.gt !== undefined && range.gte !== undefined) {
        throw new TypeError('index range cannot specify both gt and gte');
    }
    if (range.lt !== undefined && range.lte !== undefined) {
        throw new TypeError('index range cannot specify both lt and lte');
    }
}
