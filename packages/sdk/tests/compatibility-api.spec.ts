import { expect, test } from '@playwright/test';
import * as sdk from '../src/index';
import golden from './compatibility-golden.json' with { type: 'json' };
import type * as V1 from './compatibility-v1-types';

type _CompatibleTypeExports = [
    sdk.TxMode,
    sdk.TxId,
    sdk.DebugFailpoint,
    sdk.ChangeKind,
    sdk.Unsubscribe,
    sdk.BatchOp,
    sdk.MigrateHook,
    sdk.DbChange,
    sdk.DbSubscriptionCallback,
    sdk.Range,
    sdk.IndexDef,
    sdk.OpenOptions,
    sdk.ChangeFeedSettings,
    sdk.PutOptions,
    sdk.CompressionKind,
    sdk.CreateStoreOptions,
    sdk.ExportSnapshotOptions,
    sdk.ChangeRecord,
    sdk.ChangeFeedOptions,
    sdk.ChangeFeed,
    sdk.DbStats,
    sdk.EngineHealth,
    sdk.StorageInfo,
    sdk.CompactionResult,
    sdk.ScanItem,
    sdk.Transaction,
    sdk.MigrationContext,
    sdk.DB,
    sdk.CompoundKeyPart,
    sdk.IndexKeyPrimitive,
    sdk.NormalizedIndexDef,
    sdk.DecodedIndexEntryKey
];

const _compatibleOpen: (name: string, options?: V1.OpenOptions) => Promise<V1.DB> = sdk.openDB;
const _compatibleDelete: (name: string) => Promise<void> = sdk.deleteDB;
const _compatibleCrash: (name: string) => boolean = sdk.unsafeDebugCrashWorker;
const _compatibleDb = (db: sdk.DB): V1.DB => db;
const _compatibleTransaction = (transaction: sdk.Transaction): V1.Transaction => transaction;
type LegacyDbImplementation = Omit<Pick<sdk.DB, keyof V1.DB>, 'begin'> & { begin: V1.DB['begin'] };
const _compatibleDbImplementation = (db: V1.DB): LegacyDbImplementation => db;
const _compatibleTransactionImplementation = (
    transaction: V1.Transaction
): Pick<sdk.Transaction, keyof V1.Transaction> => transaction;
const _compatibleMigration: (context: sdk.MigrationContext) => V1.MigrationContext = (context) => context;

function hex(bytes: Uint8Array): string {
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

function fromHex(value: string): Uint8Array {
    return Uint8Array.from(value.match(/../g) ?? [], (pair) => Number.parseInt(pair, 16));
}

test('the package entrypoint retains every Release 1.0.1 runtime export', () => {
    const exported = new Set(Object.keys(sdk));
    expect(golden.exports.filter((name) => !exported.has(name))).toEqual([]);
});

test('Release 1.0.1 errors retain names, classes and normalization', () => {
    const constructors = sdk as unknown as Record<string, new (message: string) => Error>;
    for (const error of golden.errors) {
        const direct = new constructors[error.export]('compatibility');
        const normalized = sdk.normalizeError({ code: error.name, message: 'compatibility' });
        expect(direct.name).toBe(error.name);
        expect(normalized).toBeInstanceOf(constructors[error.export]);
        expect(normalized).toBeInstanceOf(sdk.MoyoDbError);
        expect(normalized.name).toBe(error.name);
        expect(normalized.message).toBe('compatibility');
    }
    expect(new sdk.UniqueIndexConstraintError('duplicate')).toBeInstanceOf(sdk.ConstraintError);
});

test('persisted scalar and compound keys retain Release 1.0.1 bytes', () => {
    const parts = [null, false, true, -10, -0, 1.5, '', 'a\u0000я', new Uint8Array([0, 255])];
    expect(parts.map((part) => hex(sdk.indexKey(part)))).toEqual(golden.scalarKeys);
    expect(parts.map((part) => hex(sdk.encodeIndexScalar(part)))).toEqual(golden.scalarKeys);
    expect(hex(sdk.compoundKey(...parts))).toBe(golden.compoundKey);
    expect(sdk.splitCompoundKey(fromHex(golden.compoundKey)).map(hex)).toEqual(golden.scalarKeys);
    expect(hex(sdk.encodeCompoundKeyParts([new Uint8Array([0, 255]), new Uint8Array([])]))).toBe(golden.rawCompound);
    expect(sdk.splitCompoundKey(fromHex(golden.rawCompound)).map(hex)).toEqual(['00ff', '']);
});

test('integer keys and JSON values retain Release 1.0.1 bytes', () => {
    expect([0n, 1n, 4294967296n, 18446744073709551615n].map((value) => hex(sdk.u64Key(value)))).toEqual(golden.u64Keys);
    const document = { label: 'я\u0000', nested: [null, true, 7] };
    expect(hex(sdk.jsonEncode(document))).toBe(golden.json);
    expect(sdk.jsonDecode(fromHex(golden.json))).toEqual(document);
    expect(sdk.utf8Decode(sdk.utf8Encode('Москва\u0000'))).toBe('Москва\u0000');
});

test('Release 1.0.1 index catalogs and physical keys remain readable and writable', () => {
    expect(sdk.INDEX_METADATA_STORE).toBe(golden.metadataStore);
    for (const index of golden.indexMetadata) {
        const old = sdk.decodeIndexMetadataValue(fromHex(index.value));
        const [current] = sdk.normalizeIndexDefinitions([index.public]);
        expect(sdk.toPublicIndexDefinitions([old])).toEqual([index.public]);
        expect(old.internalStore).toBe(index.internalStore);
        expect(current.internalStore).toBe(index.internalStore);
        expect(hex(sdk.encodeIndexMetadataKey(current.store, current.name))).toBe(index.key);
        expect(hex(sdk.encodeIndexMetadataValue(current))).toBe(index.value);
        const logical = sdk.extractLogicalIndexKey(
            old,
            sdk.jsonEncode({ email: 'a@example.test', address: { city: 'Москва' }, age: 42 })
        );
        expect(logical).not.toBeNull();
        expect(hex(logical as Uint8Array)).toBe(index.logicalKey);
    }
    const decoded = sdk.decodeIndexEntryKey(fromHex(golden.indexEntry));
    expect(hex(decoded.logicalKey)).toBe(golden.scalarKeys[7]);
    expect(Array.from(decoded.primaryKey)).toEqual([0, 1, 255]);
    expect(hex(sdk.encodeIndexEntryKey(sdk.indexKey('a\u0000я'), new Uint8Array([0, 1, 255])))).toBe(golden.indexEntry);
});
