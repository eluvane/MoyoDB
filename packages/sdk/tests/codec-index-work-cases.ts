import type * as Codec from '../src/codec';
import type * as Indexing from '../src/indexing';
import type * as Compression from '../src/compression';

type CodecApi = typeof Codec;
type IndexingApi = typeof Indexing;
type CompressionApi = typeof Compression;

function ok(value: unknown, message: string): asserts value {
    if (!value) {
        throw new Error(message);
    }
}

function equalBytes(actual: Uint8Array, expected: Uint8Array, message: string): void {
    ok(actual.byteLength === expected.byteLength && actual.every((byte, index) => byte === expected[index]), message);
}

function compareBytes(left: Uint8Array, right: Uint8Array): number {
    const length = Math.min(left.byteLength, right.byteLength);
    for (let index = 0; index < length; index += 1) {
        if (left[index] !== right[index]) {
            return left[index] - right[index];
        }
    }
    return left.byteLength - right.byteLength;
}

async function rejects(run: () => unknown, name: string): Promise<void> {
    try {
        await run();
    } catch (error) {
        ok(error instanceof Error && error.name === name, `expected ${name}, got ${String(error)}`);
        return;
    }
    throw new Error(`expected ${name}, input was accepted`);
}

/** These cases use production modules without storage or Worker fixtures. */
export async function checkCodecIndexWork(api: {
    codec: CodecApi;
    indexing: IndexingApi;
    compression: CompressionApi;
}): Promise<{ passed: number; failed: number; results: Array<{ name: string; passed: boolean; error?: string }> }> {
    const { codec, indexing, compression } = api;
    const results: Array<{ name: string; passed: boolean; error?: string }> = [];
    const test = async (name: string, run: () => unknown): Promise<void> => {
        try {
            await run();
            results.push({ name, passed: true });
        } catch (error) {
            results.push({ name, passed: false, error: String(error) });
        }
    };

    await test('u64 keys retain the full unsigned range and big-endian order', () => {
        equalBytes(codec.u64Key(0), new Uint8Array(8), 'zero encoding');
        equalBytes(codec.u64Key(0xffff_ffff_ffff_ffffn), new Uint8Array(8).fill(255), 'maximum encoding');
        ok(compareBytes(codec.u64Key(255), codec.u64Key(256)) < 0, 'u64 order changed');
    });
    await test('u64 keys reject bigint overflow rather than wrapping to zero', async () => {
        await rejects(() => codec.u64Key(1n << 64n), 'RangeError');
        await rejects(() => codec.u64Key(-1n), 'RangeError');
    });
    await test('u64 number inputs must be safe non-negative integers', async () => {
        for (const value of [Number.MAX_SAFE_INTEGER + 1, -1, 0.5, Number.NaN, Number.POSITIVE_INFINITY]) {
            await rejects(() => codec.u64Key(value), 'RangeError');
        }
        equalBytes(codec.u64Key(Number.MAX_SAFE_INTEGER), codec.u64Key(BigInt(Number.MAX_SAFE_INTEGER)), 'safe u64');
    });
    await test('JSON codecs roundtrip ordinary values and valid replacement characters', () => {
        for (const value of [null, false, 3, '', '\ufffd', '\ud83d\ude00', { field: ['x', 2] }]) {
            ok(JSON.stringify(codec.jsonDecode(codec.jsonEncode(value))) === JSON.stringify(value), 'JSON roundtrip');
        }
    });
    await test('JSON encoding rejects roots with no JSON representation', async () => {
        for (const value of [undefined, Symbol('not-json'), () => {}, { toJSON: () => undefined }]) {
            await rejects(() => codec.jsonEncode(value), 'TypeError');
        }
    });
    await test('JSON decoding rejects malformed UTF-8 inside otherwise valid JSON', async () => {
        for (const bytes of [Uint8Array.of(34, 255, 34), Uint8Array.of(34, 0xe2, 0x82, 34)]) {
            await rejects(() => codec.jsonDecode(bytes), 'TypeError');
        }
        ok(codec.utf8Decode(Uint8Array.of(255)) === '\ufffd', 'text decoder replacement behavior changed');
    });
    await test('numeric scalar and compound keys preserve finite number order', () => {
        const values = [-Number.MAX_VALUE, -10, -Number.MIN_VALUE, 0, Number.MIN_VALUE, 10, Number.MAX_VALUE];
        for (let index = 1; index < values.length; index += 1) {
            ok(
                compareBytes(codec.indexKey(values[index - 1]), codec.indexKey(values[index])) < 0,
                'scalar number order'
            );
            ok(
                compareBytes(codec.compoundKey('group', values[index - 1]), codec.compoundKey('group', values[index])) <
                    0,
                'compound number order'
            );
        }
        equalBytes(codec.indexKey(-0), codec.indexKey(0), 'negative zero normalization');
    });
    await test('Unicode scalar strings retain UTF-8 order, prefixes and embedded NUL bytes', () => {
        const values = ['', '\u0000', '\u0000a', 'a', 'aa', '\u00e9', '\ue000', '\ufffd', '\ud83d\ude00'];
        for (let index = 1; index < values.length; index += 1) {
            ok(
                compareBytes(codec.indexKey(values[index - 1]), codec.indexKey(values[index])) < 0,
                'UTF-8 string order'
            );
            ok(
                compareBytes(codec.compoundKey(values[index - 1], 1), codec.compoundKey(values[index], 1)) < 0,
                'compound string order'
            );
        }
    });
    await test('index strings reject lone surrogates instead of colliding with replacement characters', async () => {
        for (const value of ['\ud800', '\udfff', 'x\ud800y', '\ud800\ud800', '\udc00\ud800']) {
            await rejects(() => codec.indexKey(value), 'TypeError');
            await rejects(() => codec.compoundKey('group', value), 'TypeError');
        }
    });
    await test('compound and index entry codecs escape zero bytes and own their outputs', () => {
        const logical = Uint8Array.of(0, 255, 0, 1);
        const primary = Uint8Array.of(255, 0, 0);
        const encoded = indexing.encodeIndexEntryKey(logical, primary);
        const decoded = indexing.decodeIndexEntryKey(encoded);
        equalBytes(decoded.logicalKey, logical, 'logical key escaping');
        equalBytes(decoded.primaryKey, primary, 'primary key escaping');
        logical.fill(1);
        primary.fill(2);
        decoded.logicalKey.fill(3);
        const again = indexing.decodeIndexEntryKey(encoded);
        equalBytes(again.logicalKey, Uint8Array.of(0, 255, 0, 1), 'key ownership');
        equalBytes(again.primaryKey, Uint8Array.of(255, 0, 0), 'primary ownership');
    });
    await test('physical index ranges preserve every inclusive and exclusive logical bound', () => {
        const keys = [[], [0], [0, 0], [0, 255], [1], [255], [255, 0], [255, 255]].map((key) => Uint8Array.from(key));
        const primaries = [new Uint8Array(), Uint8Array.of(0), Uint8Array.of(255)];
        const bounds = ['gte', 'gt', 'lte', 'lt'] as const;
        for (const bound of bounds) {
            for (const key of keys) {
                const range = indexing.indexRangeToPhysicalRange({ [bound]: key, reverse: true, limit: 2 });
                ok(range.reverse === true && range.limit === 2, 'range options lost');
                for (const logical of keys) {
                    const compared = compareBytes(logical, key);
                    const expected =
                        bound === 'gte'
                            ? compared >= 0
                            : bound === 'gt'
                              ? compared > 0
                              : bound === 'lte'
                                ? compared <= 0
                                : compared < 0;
                    for (const primary of primaries) {
                        const physical = indexing.encodeIndexEntryKey(logical, primary);
                        const actual =
                            (!range.gte || compareBytes(physical, range.gte) >= 0) &&
                            (!range.lt || compareBytes(physical, range.lt) < 0);
                        ok(actual === expected, `${bound} changed logical inclusion`);
                    }
                }
            }
        }
    });
    await test('missing indexed fields do not resolve Object prototype properties', () => {
        for (const keyPath of ['toString', 'constructor.name', '__proto__.constructor.name', 'nested.toString']) {
            const [def] = indexing.normalizeIndexDefinitions([{ store: 'docs', name: 'lookup', keyPath }]);
            ok(
                indexing.extractLogicalIndexKey(def, codec.jsonEncode({ nested: {} })) === null,
                `${keyPath} was inherited`
            );
        }
    });
    await test('index identities distinguish embedded NUL names while retaining legacy hashed store names', () => {
        const defs = indexing.normalizeIndexDefinitions([
            { store: 'a\u0000b', name: 'c', keyPath: 'value' },
            { store: 'a', name: 'b\u0000c', keyPath: 'value' }
        ]);
        ok(defs.length === 2 && defs[0].internalStore !== defs[1].internalStore, 'distinct index names collided');
        const [hashed] = indexing.normalizeIndexDefinitions([
            { store: 's'.repeat(100), name: 'n'.repeat(200), keyPath: 'value' }
        ]);
        ok(hashed.internalStore === '__browserdb:index:h:7d1625c67ba33d3b', 'legacy hashed store naming changed');
    });
    await test('own JSON properties named constructor and __proto__ remain indexable', () => {
        for (const keyPath of ['constructor.name', '__proto__.name']) {
            const [def] = indexing.normalizeIndexDefinitions([{ store: 'docs', name: 'lookup', keyPath }]);
            const document = codec.utf8Encode('{"constructor":{"name":"own"},"__proto__":{"name":"own"}}');
            const key = indexing.extractLogicalIndexKey(def, document);
            ok(key !== null, 'own field was skipped');
            equalBytes(key, codec.indexKey('own'), 'own special property resolution');
        }
    });
    await test('malformed persisted index entry encodings report serialization errors', async () => {
        for (const bytes of [Uint8Array.of(1), Uint8Array.of(0), Uint8Array.of(0, 1), new Uint8Array()]) {
            await rejects(() => indexing.decodeIndexEntryKey(bytes), 'SerializationError');
        }
    });
    await test('malformed persisted index metadata reports serialization errors', async () => {
        for (const value of [null, {}, { store: 'docs', name: 'byKey', keyPath: [] }]) {
            await rejects(() => indexing.decodeIndexMetadataValue(codec.jsonEncode(value)), 'SerializationError');
        }
    });
    await test('invalid indexed scalar values report serialization errors', async () => {
        const [def] = indexing.normalizeIndexDefinitions([{ store: 'docs', name: 'lookup', keyPath: 'key' }]);
        for (const document of ['{"key":1e400}', '{"key":"\\ud800"}']) {
            await rejects(() => indexing.extractLogicalIndexKey(def, codec.utf8Encode(document)), 'SerializationError');
        }
    });
    await test('compression accepts the maximum logical value size', async () => {
        const value = new Uint8Array(8 * 1024 * 1024);
        for (const kind of ['gzip', 'deflate'] as const) {
            const encoded = await compression.encodeStoreValueRecord(value, kind);
            equalBytes(
                await compression.decodeStoreValueRecord(encoded, { strict: true }),
                value,
                `${kind} maximum value`
            );
        }
    });
    await test('compression rejects oversized logical values before starting streams', async () => {
        const OriginalCompressionStream = globalThis.CompressionStream;
        let streams = 0;
        globalThis.CompressionStream = class extends OriginalCompressionStream {
            constructor(format: CompressionFormat) {
                super(format);
                streams += 1;
            }
        };
        const oversized = new Uint8Array(8 * 1024 * 1024 + 1);
        try {
            for (const kind of ['gzip', 'deflate'] as const) {
                await rejects(() => compression.encodeStoreValueRecord(oversized, kind), 'ValueTooLargeError');
            }
            ok(streams === 0, 'oversized value started compression work');
        } finally {
            globalThis.CompressionStream = OriginalCompressionStream;
        }
    });
    await test('strict value decoding rejects truncated recognized envelopes', async () => {
        const record = await compression.encodeStoreValueRecord(Uint8Array.of(1), 'gzip');
        for (const length of [8, 9, 17]) {
            const truncated = record.slice(0, length);
            await rejects(() => compression.decodeStoreValueRecord(truncated, { strict: true }), 'CorruptionError');
            ok(
                (await compression.decodeStoreValueRecord(truncated, { strict: false })) === truncated,
                'raw fallback identity'
            );
        }
    });
    await test('snapshot decoding rejects truncated recognized envelopes', async () => {
        const snapshot = await compression.wrapSnapshotWithCompression(Uint8Array.of(1), 'gzip');
        for (const length of [8, 9, 17]) {
            await rejects(() => compression.unwrapSnapshotCompression(snapshot.slice(0, length)), 'CorruptionError');
        }
    });
    await test('legacy raw bytes and uncompressed ownership semantics remain intact', async () => {
        const raw = Uint8Array.of(1, 2, 3);
        ok((await compression.decodeStoreValueRecord(raw, { strict: true })) === raw, 'legacy raw value');
        ok((await compression.unwrapSnapshotCompression(raw)) === raw, 'raw snapshot');
        ok((await compression.encodeStoreValueRecord(raw, false)) === raw, 'raw encoding identity');
        ok((await compression.wrapSnapshotWithCompression(raw, false)) === raw, 'raw wrapper identity');
    });
    return {
        passed: results.filter((result) => result.passed).length,
        failed: results.filter((result) => !result.passed).length,
        results
    };
}
