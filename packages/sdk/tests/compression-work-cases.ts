type CompressionApi = typeof import('../src/compression');

function bytes(length: number, random: boolean): Uint8Array<ArrayBuffer> {
    const out = new Uint8Array(length);
    let state = 0x12345678;
    for (let index = 0; index < length; index += 1) {
        state ^= state << 13;
        state ^= state >>> 17;
        state ^= state << 5;
        out[index] = random ? state & 0xff : index % 7;
    }
    return out;
}

function equal(actual: Uint8Array, expected: Uint8Array, label: string): void {
    if (actual.byteLength !== expected.byteLength || actual.some((byte, index) => byte !== expected[index])) {
        throw new Error(`${label}: bytes differ`);
    }
}

async function rejectsCorruption(run: () => Promise<unknown>, label: string): Promise<void> {
    try {
        await run();
    } catch (error) {
        if (error instanceof Error && error.name === 'CorruptionError') {
            return;
        }
        throw error;
    }
    throw new Error(`${label}: corrupt input was accepted`);
}

/** Portable cases run against the real module in the browser and in an isolated harness. */
export async function checkCompressionWork(api: CompressionApi): Promise<{ cases: number; blobConstructions: number }> {
    const OriginalBlob = globalThis.Blob;
    let blobConstructions = 0;
    let cases = 0;
    globalThis.Blob = class extends OriginalBlob {
        constructor(parts?: BlobPart[], options?: BlobPropertyBag) {
            super(parts, options);
            blobConstructions += 1;
        }
    };
    try {
        for (const kind of ['gzip', 'deflate'] as const) {
            for (const length of [0, 1, 1023, 1024, 4096, 65539, 1048576]) {
                for (const random of [false, true]) {
                    const value = bytes(length, random);
                    const record = await api.encodeStoreValueRecord(value, kind);
                    equal(await api.decodeStoreValueRecord(record, { strict: true }), value, `${kind} store ${length}`);
                    const snapshot = await api.wrapSnapshotWithCompression(value, kind);
                    equal(await api.unwrapSnapshotCompression(snapshot), value, `${kind} snapshot ${length}`);
                    cases += 2;
                }
            }

            // Compression and its raw fallback must use the same snapshot of the caller's input.
            const source = bytes(2064, true);
            const input = source.subarray(8, 2056);
            const expected = input.slice();
            const pending = api.encodeStoreValueRecord(input, kind);
            source.fill(0);
            const record = await pending;
            if (record[9] !== 0) {
                throw new Error(`${kind}: fixture did not exercise the uncompressed fallback`);
            }
            equal(await api.decodeStoreValueRecord(record, { strict: true }), expected, `${kind} fallback ownership`);
            cases += 1;

            const snapshotInput = bytes(65539, false);
            const expectedSnapshot = snapshotInput.slice();
            const pendingSnapshot = api.wrapSnapshotWithCompression(snapshotInput, kind);
            structuredClone(snapshotInput.buffer, { transfer: [snapshotInput.buffer] });
            equal(
                await api.unwrapSnapshotCompression(await pendingSnapshot),
                expectedSnapshot,
                `${kind} detached snapshot input`
            );
            cases += 1;

            const compressed = await api.encodeStoreValueRecord(bytes(4096, false), kind);
            const pendingDecode = api.decodeStoreValueRecord(compressed, { strict: true });
            structuredClone(compressed.buffer, { transfer: [compressed.buffer] });
            equal(await pendingDecode, bytes(4096, false), `${kind} detached decode input`);
            cases += 1;

            const valid = await api.encodeStoreValueRecord(bytes(4096, false), kind);
            const badChecksum = valid.slice();
            badChecksum[18] ^= 0xff;
            await rejectsCorruption(() => api.decodeStoreValueRecord(badChecksum, { strict: true }), 'checksum');
            if ((await api.decodeStoreValueRecord(badChecksum, { strict: false })) !== badChecksum) {
                throw new Error('non-strict fallback lost its identity');
            }
            const shortLength = valid.slice();
            new DataView(shortLength.buffer).setUint32(10, 8, true);
            await rejectsCorruption(() => api.decodeStoreValueRecord(shortLength, { strict: true }), 'output bound');
            const oversized = valid.slice();
            new DataView(oversized.buffer).setUint32(10, 8 * 1024 * 1024 + 1, true);
            await rejectsCorruption(() => api.decodeStoreValueRecord(oversized, { strict: true }), 'value size limit');
            const snapshot = await api.wrapSnapshotWithCompression(bytes(4096, false), kind);
            new DataView(snapshot.buffer).setUint32(10, 256 * 1024 * 1024 + 1, true);
            await rejectsCorruption(() => api.unwrapSnapshotCompression(snapshot), 'snapshot size limit');
            cases += 5;
        }
        const plain = bytes(64, false);
        if ((await api.encodeStoreValueRecord(plain, false)) !== plain) {
            throw new Error('uncompressed store changed ownership semantics');
        }
        if ((await api.wrapSnapshotWithCompression(plain, false)) !== plain) {
            throw new Error('uncompressed snapshot changed ownership semantics');
        }
        cases += 2;
        if (blobConstructions !== 0) {
            throw new Error(`compression made ${blobConstructions} redundant Blob snapshots`);
        }
        return { cases, blobConstructions };
    } finally {
        globalThis.Blob = OriginalBlob;
    }
}
