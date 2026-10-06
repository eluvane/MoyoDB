import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import ts from 'typescript';

async function compile(name) {
    const source = await readFile(new URL(`../src/${name}.ts`, import.meta.url), 'utf8');
    const result = ts.transpileModule(source, {
        fileName: `${name}.ts`,
        reportDiagnostics: true,
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
    });
    assert.deepEqual(
        (result.diagnostics ?? []).filter((diagnostic) => diagnostic.category === ts.DiagnosticCategory.Error),
        []
    );
    return result.outputText;
}

const moduleUrl = (source) => `data:text/javascript;base64,${Buffer.from(source).toString('base64')}`;
const snappyUrl = moduleUrl(await compile('snappy'));
const codecUrl = moduleUrl(await compile('codec'));
const { encodeSnappy, decodeSnappy } = await import(snappyUrl);
const compressionSource = (await compile('compression'))
    .replace("from './snappy'", `from '${snappyUrl}'`)
    .replace("from './codec'", `from '${codecUrl}'`);
const {
    encodeStoreValueRecord,
    decodeStoreValueRecord,
    compressionFromStoreFlags,
    prepareFastStoreValueRecord,
    writePreparedStoreValueRecord
} = await import(moduleUrl(compressionSource));

const text = (value) => new TextEncoder().encode(value);
const bytes = (...values) => Uint8Array.from(values);
const corruption = { name: 'CorruptionError' };

function randomBytes(length, seed = 0x12345678) {
    const output = new Uint8Array(length);
    let state = seed;
    for (let index = 0; index < length; index += 1) {
        state ^= state << 13;
        state ^= state >>> 17;
        state ^= state << 5;
        output[index] = state & 0xff;
    }
    return output;
}

function crc32Bitwise(value) {
    let crc = 0xffffffff;
    for (const byte of value) {
        crc ^= byte;
        for (let bit = 0; bit < 8; bit += 1) {
            crc = (crc >>> 1) ^ ((crc & 1) === 0 ? 0 : 0xedb88320);
        }
    }
    return (crc ^ 0xffffffff) >>> 0;
}

test('store CRC32 matches legacy vectors for offsets, tails and large entropy', async () => {
    const vectors = [
        [new Uint8Array(), 0],
        [text('123456789'), 0xcbf43926],
        [text('hello world'), 0x0d4a1185],
        [text('The quick brown fox jumps over the lazy dog'), 0x414fa339],
        [Uint8Array.from({ length: 256 }, (_, index) => index), 0x29058c73]
    ];
    for (const [value, expected] of vectors) {
        assert.equal(crc32Bitwise(value), expected);
        const record = await encodeStoreValueRecord(value, 'snappy');
        assert.equal(new DataView(record.buffer, record.byteOffset).getUint32(14, true), expected);
        assert.deepEqual(await decodeStoreValueRecord(record, { strict: true }), value);
    }
    assert.equal(
        Buffer.from(await encodeStoreValueRecord(text('123456789'), 'snappy')).toString('hex'),
        '4244425a56414c310100090000002639f4cb313233343536373839'
    );
    for (let length = 0; length < 96; length += 1) {
        for (let sourceOffset = 0; sourceOffset < 16; sourceOffset += 1) {
            const source = randomBytes(length + 31, 1 + length * 31 + sourceOffset);
            const value = source.subarray(sourceOffset, sourceOffset + length);
            const prepared = prepareFastStoreValueRecord(value, 'snappy');
            const targetOffset = (length + sourceOffset) % 17;
            const storage = new Uint8Array(prepared.byteLength + targetOffset + 23).fill(255);
            const target = storage.subarray(3);
            const end = writePreparedStoreValueRecord(prepared, target, targetOffset);
            const record = target.subarray(targetOffset, end);
            assert.equal(
                new DataView(record.buffer, record.byteOffset).getUint32(14, true),
                crc32Bitwise(value),
                `length ${length}, source offset ${sourceOffset}, target offset ${targetOffset}`
            );
            assert.ok(storage.subarray(0, 3 + targetOffset).every((byte) => byte === 255));
            assert.ok(storage.subarray(3 + end).every((byte) => byte === 255));
        }
    }
    for (let seed = 1; seed <= 180; seed += 1) {
        const value = randomBytes((seed * 1543) % 32773, seed);
        const record = await encodeStoreValueRecord(value, 'snappy');
        assert.equal(
            new DataView(record.buffer, record.byteOffset).getUint32(14, true),
            crc32Bitwise(record.subarray(18)),
            `random seed ${seed}`
        );
    }
    // Large checksums were computed independently with Python zlib.crc32.
    for (const [length, checksum] of [
        [1024 * 1024, 0xe6568d53],
        [8 * 1024 * 1024, 0xf1e9a5ef]
    ]) {
        const value = randomBytes(length);
        const encoded = await encodeStoreValueRecord(value, 'snappy');
        assert.equal(encoded[9], 0);
        const shifted = new Uint8Array(encoded.byteLength + 23);
        shifted.set(encoded, 11);
        const record = shifted.subarray(11, 11 + encoded.byteLength);
        assert.equal(new DataView(record.buffer, record.byteOffset).getUint32(14, true), checksum);
        assert.deepEqual(await decodeStoreValueRecord(record, { strict: true }), value);
        for (const offset of [0, 7, 8, 9, length - 1]) {
            record[18 + offset] ^= 1;
            await assert.rejects(decodeStoreValueRecord(record, { strict: true }), corruption);
            assert.equal(await decodeStoreValueRecord(record, { strict: false }), record);
            record[18 + offset] ^= 1;
        }
    }
});

test('raw Snappy matches independent golden blocks and supports all copy types', () => {
    // Vectors: https://github.com/golang/snappy/blob/master/snappy_test.go
    const literal = [...text('abcd')];
    const vectors = [
        [bytes(0), new Uint8Array()],
        [bytes(4, 12, ...literal), text('abcd')],
        [bytes(13, 12, ...literal, 21, 4), text('abcdabcdabcda')],
        [bytes(8, 12, ...literal, 1, 1), text('abcddddd')],
        [bytes(6, 12, ...literal, 6, 3, 0), text('abcdbc')],
        [bytes(6, 12, ...literal, 7, 3, 0, 0, 0), text('abcdbc')],
        [bytes(150, 1, 0, 65, 254, 1, 0, 254, 1, 0, 82, 1, 0), text('A'.repeat(150))],
        [
            bytes(112, 0, 66, 238, 1, 0, 13, 1, 8, 101, 102, 67, 78, 1, 0, 78, 90, 0, 0, 103),
            text(`${'B'.repeat(68)}ef${'C'.repeat(21)}${'B'.repeat(20)}g`)
        ]
    ];
    for (const [encoded, expected] of vectors) {
        assert.deepEqual(decodeSnappy(encoded, expected.byteLength), expected);
    }
    assert.deepEqual(encodeSnappy(text('A'.repeat(150))), vectors[6][0]);
    assert.deepEqual(encodeSnappy(text('abcd')), vectors[1][0]);
    for (let lengthBytes = 1; lengthBytes <= 4; lengthBytes += 1) {
        const encoded = bytes(3, (59 + lengthBytes) << 2, 2, ...new Array(lengthBytes - 1).fill(0), 255, 255, 255);
        assert.deepEqual(decodeSnappy(encoded, 3), bytes(255, 255, 255));
    }
});

test('raw Snappy reads 32-bit offsets beyond the encoder block size', () => {
    const dots = text('.'.repeat(65536));
    const encoded = bytes(137, 128, 4, 12, ...text('pqrs'), 244, 255, 255, ...dots, 19, 4, 0, 1, 0);
    assert.deepEqual(decodeSnappy(encoded, 65545), text(`pqrs${'.'.repeat(65536)}pqrs.`));
});

test('raw Snappy roundtrips block, literal and varint boundaries with deterministic random data', () => {
    const lengths = [0, 1, 3, 4, 11, 60, 61, 127, 128, 255, 256, 1023, 1024, 16383, 16384, 65535, 65536, 65537, 131073];
    for (const length of lengths) {
        for (const mode of ['noise', 'repeat', 'mixed']) {
            const value = randomBytes(length);
            for (let index = 0; index < length; index += 1) {
                if (mode === 'repeat' || (mode === 'mixed' && index % 4096 >= 2048)) {
                    value[index] = index % 7;
                }
            }
            const original = value.slice();
            const storage = new Uint8Array(length + 17);
            storage.set(value, 9);
            const encoded = encodeSnappy(storage.subarray(9, 9 + length));
            assert.deepEqual(decodeSnappy(encoded, length, length), original, `${mode}: ${length}`);
            assert.deepEqual(value, original);
            assert.ok(encoded.byteLength <= length + Math.ceil(length / 6) + 32);
        }
    }
    for (let seed = 1; seed <= 300; seed += 1) {
        const length = (seed * 7919) % 16389;
        const value = randomBytes(length, seed);
        const period = 1 + (seed % 31);
        for (let index = Math.floor(length / 2); index < length; index += 1) {
            value[index] = value[index % period];
        }
        assert.deepEqual(decodeSnappy(encodeSnappy(value), length), value, `seed ${seed}`);
    }
});

test('raw Snappy rejects hostile lengths, offsets, truncation and trailing data', () => {
    const invalid = [
        [],
        [128],
        [128, 128, 128, 128, 16],
        [255, 255, 255, 255, 255, 0],
        [1, 240],
        [1, 244, 0],
        [1, 248, 0, 0],
        [1, 252, 0, 0, 0],
        [1, 252, 255, 255, 255, 255],
        [3, 8, 255, 255],
        [2, 8, 255, 255, 255],
        [4, 1],
        [4, 2, 0],
        [4, 3, 0, 0, 0],
        [4, 1, 1],
        [8, 12, 97, 98, 99, 100, 1, 0],
        [8, 12, 97, 98, 99, 100, 1, 5],
        [7, 12, 97, 98, 99, 100, 1, 4],
        [9, 12, 97, 98, 99, 100, 1, 4],
        [0, 0, 65],
        [1, 0, 65, 0, 66]
    ];
    for (const value of invalid) {
        assert.throws(() => decodeSnappy(Uint8Array.from(value), 1024), corruption);
    }
    const encoded = encodeSnappy(text('abcd'.repeat(80)));
    for (let length = 0; length < encoded.byteLength; length += 1) {
        assert.throws(() => decodeSnappy(encoded.subarray(0, length), 320), corruption);
    }
    assert.throws(() => decodeSnappy(bytes(255, 255, 255, 255, 15), 8 * 1024 * 1024), corruption);
    assert.throws(() => decodeSnappy(bytes(0), 0, 1), corruption);
    assert.throws(() => decodeSnappy(bytes(0), -1), RangeError);
});

test('Snappy store records need no compression streams and own input before suspension', async () => {
    const streams = [globalThis.CompressionStream, globalThis.DecompressionStream];
    globalThis.CompressionStream = undefined;
    globalThis.DecompressionStream = undefined;
    try {
        assert.equal(compressionFromStoreFlags(13), 'snappy');
        for (const random of [false, true]) {
            const value = random ? randomBytes(4096) : text('abcd'.repeat(1024));
            const expected = value.slice();
            const pending = encodeStoreValueRecord(value, 'snappy');
            value.fill(255);
            const record = await pending;
            assert.equal(record[9], random ? 0 : 3);
            const decoded = decodeStoreValueRecord(record, { strict: true });
            structuredClone(record.buffer, { transfer: [record.buffer] });
            assert.deepEqual(await decoded, expected);
        }
        const value = text('abcd'.repeat(1024));
        const pending = encodeStoreValueRecord(value, 'snappy');
        structuredClone(value.buffer, { transfer: [value.buffer] });
        assert.deepEqual(await decodeStoreValueRecord(await pending, { strict: true }), text('abcd'.repeat(1024)));
    } finally {
        [globalThis.CompressionStream, globalThis.DecompressionStream] = streams;
    }
});

test('store profitability includes the envelope and requires at least ten percent saving', async () => {
    let compressedCases = 0;
    let weakSavingCases = 0;
    for (let tail = 0; tail <= 400; tail += 1) {
        const value = randomBytes(1280);
        value.fill(0, value.byteLength - tail);
        const compressedSize = encodeSnappy(value).byteLength + 18;
        const expectedTag = compressedSize * 100 <= value.byteLength * 90 ? 3 : 0;
        const record = await encodeStoreValueRecord(value, 'snappy');
        assert.equal(record[9], expectedTag, `tail ${tail}, compressed size ${compressedSize}`);
        assert.deepEqual(await decodeStoreValueRecord(record, { strict: true }), value);
        if (expectedTag === 3) {
            compressedCases += 1;
        } else if (compressedSize < value.byteLength) {
            weakSavingCases += 1;
        }
    }
    assert.ok(compressedCases > 0);
    assert.ok(weakSavingCases > 0);
    const exact = randomBytes(1110);
    exact.fill(0, 959);
    assert.equal(encodeSnappy(exact).byteLength + 18, 999);
    assert.equal((await encodeStoreValueRecord(exact, 'snappy'))[9], 3);
    const small = new Uint8Array(1023);
    assert.equal((await encodeStoreValueRecord(small, 'snappy'))[9], 0);
    await assert.rejects(encodeStoreValueRecord(new Uint8Array(8 * 1024 * 1024 + 1), 'snappy'), {
        name: 'ValueTooLargeError'
    });
});

test('Snappy envelopes keep checksum, raw length and maximum output checks', async () => {
    const value = text('abcd'.repeat(1024));
    const record = await encodeStoreValueRecord(value, 'snappy');
    const checksum = record.slice();
    checksum[18] ^= 255;
    await assert.rejects(decodeStoreValueRecord(checksum, { strict: true }), corruption);
    assert.equal(await decodeStoreValueRecord(checksum, { strict: false }), checksum);
    for (const length of [8, value.byteLength + 1, 8 * 1024 * 1024 + 1]) {
        const incorrect = record.slice();
        new DataView(incorrect.buffer).setUint32(10, length, true);
        await assert.rejects(decodeStoreValueRecord(incorrect, { strict: true }), corruption);
    }
    const maximum = new Uint8Array(8 * 1024 * 1024);
    assert.deepEqual(
        await decodeStoreValueRecord(await encodeStoreValueRecord(maximum, 'snappy'), { strict: true }),
        maximum
    );
    const maximumNoise = randomBytes(maximum.byteLength);
    const rawMaximumRecord = await encodeStoreValueRecord(maximumNoise, 'snappy');
    assert.equal(rawMaximumRecord[9], 0);
    assert.equal(rawMaximumRecord.byteLength, maximum.byteLength + 18);
    assert.deepEqual(await decodeStoreValueRecord(rawMaximumRecord, { strict: true }), maximumNoise);
});

test('prepared fast records fill one output with borrowed raw payloads and owned compressed payloads', async () => {
    const values = [text('small'), randomBytes(4096), text('abcd'.repeat(1024))];
    const records = values.map((value) => prepareFastStoreValueRecord(value, 'snappy'));
    assert.equal(records[0].payload, values[0]);
    assert.equal(records[1].payload, values[1]);
    assert.notEqual(records[2].payload, values[2]);
    const target = new Uint8Array(records.reduce((length, record) => length + record.byteLength, 17)).fill(255);
    let offset = 9;
    for (const [index, record] of records.entries()) {
        const end = writePreparedStoreValueRecord(record, target, offset);
        assert.equal(end, offset + record.byteLength);
        assert.deepEqual(target.subarray(offset, end), await encodeStoreValueRecord(values[index], 'snappy'));
        assert.deepEqual(await decodeStoreValueRecord(target.subarray(offset, end), { strict: true }), values[index]);
        offset = end;
    }
    assert.ok(target.subarray(0, 9).every((byte) => byte === 255));
    assert.ok(target.subarray(offset).every((byte) => byte === 255));
    const plain = prepareFastStoreValueRecord(values[0], false);
    assert.equal(plain.payload, values[0]);
    assert.equal(plain.byteLength, values[0].byteLength);
    assert.equal(writePreparedStoreValueRecord(plain, target, 1), 6);
    assert.deepEqual(target.subarray(1, 6), values[0]);
    const expected = target.slice();
    for (const invalidOffset of [-1, 0.5, target.byteLength]) {
        assert.throws(() => writePreparedStoreValueRecord(records[0], target, invalidOffset), {
            name: 'InternalError'
        });
        assert.deepEqual(target, expected);
    }
    for (const value of [text('small'), text('abcd'.repeat(1024))]) {
        const storage = new Uint8Array(value.byteLength + 18);
        storage.set(value);
        const prepared = prepareFastStoreValueRecord(storage.subarray(0, value.byteLength), 'snappy');
        const overlapping = new Uint8Array(prepared.payload.buffer);
        const destination = overlapping.byteLength >= prepared.byteLength ? overlapping : storage;
        const end = writePreparedStoreValueRecord(prepared, destination, 0);
        assert.deepEqual(await decodeStoreValueRecord(destination.subarray(0, end), { strict: true }), value);
    }
});

test('Snappy preflight skips full high-entropy encoding and retains profitable distributed data', async () => {
    const observedUrl = moduleUrl(`
import { encodeSnappy as encode, decodeSnappy } from '${snappyUrl}';
export { decodeSnappy };
export const encodedLengths = [];
export function encodeSnappy(input) {
    encodedLengths.push(input.byteLength);
    return encode(input);
}
`);
    const { encodedLengths } = await import(observedUrl);
    const observed = await import(moduleUrl(compressionSource.replace(snappyUrl, observedUrl)));
    assert.equal(observed.STORE_VALUE_COMPRESSION_POLICY_VERSION, 2);
    assert.equal(observed.STORE_VALUE_COMPRESSION_PREFLIGHT_MIN_BYTES, 65536);
    assert.equal(observed.STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_BYTES, 1024);
    assert.equal(observed.STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_COUNT, 3);
    assert.equal(observed.STORE_VALUE_COMPRESSION_ENVELOPE_BYTES, 18);
    for (const length of [65536, 1024 * 1024, 8 * 1024 * 1024]) {
        const value = randomBytes(length);
        encodedLengths.length = 0;
        const prepared = observed.prepareFastStoreValueRecord(value, 'snappy');
        assert.deepEqual(encodedLengths, [1024, 1024, 1024]);
        assert.equal(prepared.kindTag, 0);
        assert.equal(prepared.payload, value);
    }
    const belowMinimum = randomBytes(65535);
    encodedLengths.length = 0;
    observed.prepareFastStoreValueRecord(belowMinimum, 'snappy');
    assert.deepEqual(encodedLengths, [belowMinimum.byteLength]);
    for (const mixed of [false, true]) {
        const value = randomBytes(1024 * 1024);
        for (let index = mixed ? value.byteLength / 2 : 0; index < value.byteLength; index += 1) {
            value[index] = index & 255;
        }
        encodedLengths.length = 0;
        const prepared = observed.prepareFastStoreValueRecord(value, 'snappy');
        assert.deepEqual(encodedLengths, [1024, 1024, 1024, value.byteLength]);
        assert.equal(prepared.kindTag, 3);
        const record = new Uint8Array(prepared.byteLength);
        observed.writePreparedStoreValueRecord(prepared, record, 0);
        assert.deepEqual(await observed.decodeStoreValueRecord(record, { strict: true }), value);
    }
});
