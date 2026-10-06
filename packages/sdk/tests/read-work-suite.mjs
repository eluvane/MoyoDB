// Fixture results model the independent ownership of wasm-bindgen values.
// Compression uses the production module.
export function createReadSuite({ runtime, compression }) {
    const tests = [];
    const test = (name, run, structural = false) => tests.push({ name, run, structural });
    const ok = (value, message) => {
        if (!value) throw new Error(message);
    };
    const eq = (actual, expected, message = 'values differ') => {
        if (JSON.stringify(actual) !== JSON.stringify(expected))
            throw new Error(`${message}: ${JSON.stringify(actual)} != ${JSON.stringify(expected)}`);
    };
    const bytes = (value) => (value === null || value === undefined ? value : Array.from(value));
    const named = (name, message = name) => Object.assign(new Error(message), { name });
    const paddingView = (value) => {
        const backing = new Uint8Array(value.byteLength + 2);
        backing.set(value, 1);
        return { backing, value: backing.subarray(1, backing.byteLength - 1) };
    };
    async function rejects(run, name, message) {
        try {
            await run();
        } catch (error) {
            eq(error.name, name);
            if (message) ok(error.message.includes(message), error.message);
            return error;
        }
        throw new Error(`expected ${name}`);
    }
    function packOptional(values) {
        const metadata = 4 + values.length * 4;
        const out = new Uint8Array(metadata + values.reduce((sum, value) => sum + (value?.byteLength ?? 0), 0));
        const view = new DataView(out.buffer);
        view.setUint32(0, values.length, true);
        let offset = metadata;
        for (let index = 0; index < values.length; index++) {
            const value = values[index];
            view.setUint32(4 + index * 4, value === null ? 0xffff_ffff : value.byteLength, true);
            if (value !== null) {
                out.set(value, offset);
                offset += value.byteLength;
            }
        }
        return out;
    }
    function fixture({ values = [], rows = [], feed = [], kind = false } = {}) {
        const worker = new runtime.constructor();
        worker.committedIndexes = [];
        worker.committedStoreCompression = new Map([
            ['docs', kind],
            ['other', false]
        ]);
        const options = [];
        worker.engine = {
            needs_recovery: () => false,
            get_many: () => values,
            get_many_packed: () => packOptional(values),
            scan: () => rows,
            changes_since(_id, request) {
                options.push(request);
                let selected = request.stores ? feed.filter((change) => request.stores.includes(change.store)) : feed;
                if (request.limit !== undefined) selected = selected.slice(0, request.limit);
                return { latestTxId: 77n, changes: selected };
            }
        };
        return { worker, options, close: () => worker.persistenceBridge.close() };
    }
    let encoded;
    async function records() {
        if (!encoded) {
            encoded = [];
            for (let index = 0; index < 16; index++) {
                // Threshold is the smallest input that still takes the gzip path.
                const raw = new Uint8Array(compression.STORE_VALUE_COMPRESSION_THRESHOLD).fill(index + 1);
                encoded.push({ raw, record: await compression.encodeStoreValueRecord(raw, 'gzip') });
                ok(encoded[index].record[9] === 1, 'fixture must contain compressed gzip records');
            }
        }
        return encoded;
    }
    async function countedDecompression(run) {
        const Original = globalThis.DecompressionStream;
        const counts = { active: 0, peak: 0, starts: 0 };
        globalThis.DecompressionStream = function CountedDecompressionStream(kind) {
            const stream = new Original(kind);
            const reader = stream.readable.getReader();
            let finished = false;
            const finish = () => {
                if (!finished) {
                    finished = true;
                    counts.active--;
                }
            };
            counts.starts++;
            counts.active++;
            counts.peak = Math.max(counts.peak, counts.active);
            this.writable = stream.writable;
            this.readable = new ReadableStream({
                async pull(controller) {
                    try {
                        const next = await reader.read();
                        if (next.done) {
                            finish();
                            reader.releaseLock();
                            controller.close();
                        } else {
                            controller.enqueue(next.value);
                        }
                    } catch (error) {
                        finish();
                        reader.releaseLock();
                        controller.error(error);
                    }
                },
                async cancel(reason) {
                    try {
                        await reader.cancel(reason);
                    } finally {
                        finish();
                        reader.releaseLock();
                    }
                }
            });
        };
        try {
            return { result: await run(), ...counts };
        } finally {
            globalThis.DecompressionStream = Original;
        }
    }
    async function read(f, method, count) {
        const keys = Array.from({ length: count }, (_, index) => new Uint8Array([index]));
        if (method === 'scan') return (await f.worker.scan(1, 'docs', {})).map((row) => row.value);
        if (method === 'getManyPacked') return f.worker.getManyPacked(1, 'docs', new Uint8Array());
        return f.worker.getMany(1, 'docs', keys);
    }

    test(
        'raw scans reuse owned engine rows and normalize all byte representations',
        async () => {
            const view = paddingView(new Uint8Array([3, 4]));
            const owned = new Uint8Array([5, 6]);
            const rows = [
                { key: [1, 2], value: view.value },
                { key: owned, value: new Uint8Array([7, 8]) }
            ];
            const originalRows = rows.slice();
            const f = fixture({ rows });
            try {
                const got = await f.worker.scan(1, 'docs', {});
                eq(
                    got.map((row) => [bytes(row.key), bytes(row.value)]),
                    [
                        [
                            [1, 2],
                            [3, 4]
                        ],
                        [
                            [5, 6],
                            [7, 8]
                        ]
                    ]
                );
                ok(got[0].value.buffer !== view.backing.buffer, 'subview escaped normalization without ownership');
                ok(got[1].key === owned, 'an already owned key was copied');
                got[0].value.fill(0);
                eq(bytes(view.value), [3, 4], 'returned value mutation changed a borrowed input');
                ok(got === rows, 'raw scan allocated another row array');
                ok(
                    got.every((row, index) => row === originalRows[index]),
                    'raw scan allocated row wrappers'
                );
            } finally {
                f.close();
            }
        },
        true
    );
    test('raw scan retains key-before-value normalization and engine error order', async () => {
        const visits = [];
        const bad = {
            get length() {
                visits.push('key');
                throw named('SerializationError', 'first key');
            }
        };
        const rows = [
            {
                key: bad,
                value: {
                    get length() {
                        visits.push('value');
                        return 0;
                    }
                }
            }
        ];
        const f = fixture({ rows });
        try {
            await rejects(() => f.worker.scan(1, 'docs', {}), 'SerializationError', 'first key');
            eq(visits, ['key']);
            f.worker.engine.scan = () => {
                throw named('StoreNotFoundError');
            };
            await rejects(() => f.worker.scan(1, 'docs', {}), 'StoreNotFoundError');
            eq(visits, ['key']);
        } finally {
            f.close();
        }
    });
    for (const limit of [0, 1, undefined]) {
        test(
            `change feed limits output key copies while validating the full visible tail, limit=${limit}`,
            async () => {
                let keyCopies = 0;
                const changes = Array.from({ length: 4 }, (_, index) => {
                    const key = paddingView(new Uint8Array([index + 1])).value;
                    const slice = key.slice;
                    key.slice = function (...args) {
                        keyCopies++;
                        return slice.apply(this, args);
                    };
                    return {
                        txId: BigInt(index + 1),
                        store: 'docs',
                        key,
                        kind: 'put',
                        value: new Uint8Array([index + 1])
                    };
                });
                const feed = [{ ...changes[0], store: '__browserdb:indexes' }, ...changes];
                const f = fixture({ feed });
                const decoded = [];
                const decode = f.worker.decodeStoreValueForFeed;
                f.worker.decodeStoreValueForFeed = function (store, value) {
                    decoded.push(value[0]);
                    return decode.call(this, store, value);
                };
                try {
                    const got = await f.worker.changesSince(0, limit === undefined ? {} : { limit });
                    const expected = limit === undefined ? 4 : limit;
                    eq(got.latestTxId, 77);
                    eq(got.changes.length, expected);
                    eq(
                        got.changes.map((change) => bytes(change.value)),
                        changes.slice(0, expected).map((c) => bytes(c.value))
                    );
                    eq(decoded, [1, 2, 3, 4], 'visible tail values stopped being validated');
                    eq(f.options[0].limit, undefined, 'unscoped feed changed the engine limit');
                    eq(keyCopies, expected, 'keys outside the output limit were copied');
                } finally {
                    f.close();
                }
            },
            true
        );
    }
    test('feed store filtering, absent values and output order remain unchanged', async () => {
        const feed = [
            { txId: 1n, store: 'other', key: new Uint8Array([9]), kind: 'put', value: new Uint8Array([9]) },
            { txId: 2n, store: 'docs', key: new Uint8Array([2]), kind: 'delete' },
            { txId: 3n, store: 'docs', key: new Uint8Array(), kind: 'clear' },
            { txId: 4n, store: 'docs', key: new Uint8Array(), kind: 'drop' }
        ];
        const f = fixture({ feed });
        try {
            const got = await f.worker.changesSince(0, { stores: ['docs'], limit: 2 });
            eq(
                got.changes.map((c) => [c.txId, c.kind, bytes(c.key), c.value]),
                [
                    [2, 'delete', [2], undefined],
                    [3, 'clear', [], undefined]
                ]
            );
            eq(f.options[0], { stores: ['docs'], limit: 2 });
            eq(got.latestTxId, 77);
        } finally {
            f.close();
        }
    });
    for (const limit of [0, 1]) {
        test(`feed preserves a corrupt compressed tail beyond output limit=${limit}`, async () => {
            const inputs = await records();
            const corrupt = inputs[1].record.slice();
            corrupt[18] ^= 0xff;
            const feed = [inputs[0].record, corrupt].map((value, index) => ({
                txId: BigInt(index + 1),
                store: 'docs',
                key: new Uint8Array([index]),
                kind: 'put',
                value
            }));
            const f = fixture({ feed, kind: 'gzip' });
            try {
                await rejects(
                    () => f.worker.changesSince(0, { limit }),
                    'CorruptionError',
                    'value record checksum mismatch'
                );
            } finally {
                f.close();
            }
        });
    }
    for (const method of ['getMany', 'getManyPacked', 'scan']) {
        test(
            `${method} bounds compressed decoding and snapshots all borrowed inputs before waiting`,
            async () => {
                const inputs = await records();
                const views = inputs.map(({ record }) => paddingView(record));
                const values = views.map((view) => view.value);
                const rows = values.map((value, index) => ({ key: new Uint8Array([index]), value }));
                const f = fixture({ values, rows, kind: 'gzip' });
                try {
                    const c = await countedDecompression(async () => {
                        const pending = read(f, method, values.length);
                        for (const view of views) view.backing.fill(0);
                        return pending;
                    });
                    eq(
                        c.result.map(bytes),
                        inputs.map(({ raw }) => bytes(raw)),
                        'compressed output order or bytes changed'
                    );
                    eq(c.starts, 16);
                    eq(c.active, 0, 'successful read left an active decompressor');
                    ok(c.peak <= 8, `too many concurrent decompressions: ${c.peak}`);
                } finally {
                    f.close();
                }
            },
            true
        );
        for (const corruptIndex of [0, 15]) {
            test(
                `${method} preserves checksum error at index=${corruptIndex} and drains active decoders`,
                async () => {
                    const inputs = await records();
                    const values = inputs.map(({ record }) => record.slice());
                    values[corruptIndex][18] ^= 0xff;
                    const rows = values.map((value, index) => ({ key: new Uint8Array([index]), value }));
                    const f = fixture({ values, rows, kind: 'gzip' });
                    try {
                        const c = await countedDecompression(() =>
                            rejects(
                                () => read(f, method, values.length),
                                'CorruptionError',
                                'value record checksum mismatch'
                            )
                        );
                        eq(c.active, 0, 'failed read left an active decompressor');
                        ok(c.peak <= 8, `too many concurrent decompressions: ${c.peak}`);
                        if (corruptIndex === 0) ok(c.starts <= 7, `failed read started queued decoders: ${c.starts}`);
                    } finally {
                        f.close();
                    }
                },
                true
            );
        }
        test(`${method} preserves missing, empty and duplicate values in caller order`, async () => {
            const inputs = await records();
            const values = [
                inputs[3].record.slice(),
                null,
                inputs[1].record.slice(),
                inputs[3].record.slice(),
                new Uint8Array()
            ];
            const rows = values
                .filter((v) => v !== null)
                .map((value, index) => ({ key: new Uint8Array([index]), value }));
            const f = fixture({ values, rows, kind: 'gzip' });
            try {
                const got = await read(f, method, values.length);
                const expected = [inputs[3].raw, null, inputs[1].raw, inputs[3].raw, new Uint8Array()];
                eq(got.map(bytes), (method === 'scan' ? expected.filter((v) => v !== null) : expected).map(bytes));
                got[0].fill(0);
                eq(
                    bytes(got[method === 'scan' ? 2 : 3]),
                    bytes(inputs[3].raw),
                    'duplicate outputs share mutable bytes'
                );
            } finally {
                f.close();
            }
        });
    }
    test('standalone codec keeps its input snapshot and corruption classification', async () => {
        const inputs = await records();
        const record = inputs[0].record.slice();
        const pending = compression.decodeStoreValueRecord(record, { strict: true });
        structuredClone(record.buffer, { transfer: [record.buffer] });
        eq(bytes(await pending), bytes(inputs[0].raw));
        const invalid = inputs[1].record.slice();
        new DataView(invalid.buffer).setUint32(10, 8, true);
        await rejects(
            () => compression.decodeStoreValueRecord(invalid, { strict: true }),
            'CorruptionError',
            'exceeds 8 byte limit'
        );
    });
    return {
        async runTests(structural = true) {
            const results = [];
            for (const t of tests) {
                if (!structural && t.structural) continue;
                try {
                    await t.run();
                    results.push({ name: t.name, passed: true });
                } catch (error) {
                    results.push({ name: t.name, passed: false, error: String(error.stack ?? error) });
                }
            }
            return {
                passed: results.filter((r) => r.passed).length,
                failed: results.filter((r) => !r.passed).length,
                results
            };
        }
    };
}
