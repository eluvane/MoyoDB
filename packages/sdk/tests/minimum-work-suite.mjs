// Runs in Node and Chromium. The WASM boundary is an ordered in-memory fixture;
// no assertions here imply disk durability, Rust work counts or OPFS latency.
export function createSuite({ runtime, indexing, codec }) {
    const { jsonEncode, indexKey, u64Key } = codec;
    const empty = new Uint8Array();
    const eq = (a, b, message = 'values differ') => {
        if (JSON.stringify(a) !== JSON.stringify(b))
            throw new Error(`${message}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`);
    };
    const ok = (condition, message = 'assertion failed') => { if (!condition)
        throw new Error(message); };
    const bytes = (v) => v === null ? null : Array.from(v);
    const cmp = (a, b) => {
        for (let i = 0; i < Math.min(a.length, b.length); i++)
            if (a[i] !== b[i])
                return a[i] - b[i];
        return a.length - b.length;
    };
    const hex = (v) => Array.from(v, (b) => b.toString(16).padStart(2, '0')).join('');
    const named = (name) => Object.assign(new Error(name), { name });
    async function rejects(fn, name, fragment) {
        try {
            await fn();
        }
        catch (error) {
            eq(error.name, name);
            if (fragment)
                ok(error.message.includes(fragment), error.message);
            return;
        }
        throw new Error(`expected ${name}`);
    }
    function fixture({ stores = 1, defs = [], document = null, rows = [], recordWrites = true } = {}) {
        const worker = new runtime.constructor();
        const configs = new Map(Array.from({ length: stores }, (_, i) => [`s${i}`, false]));
        configs.set('docs', false);
        worker.committedStoreCompression = configs;
        worker.committedIndexes = defs;
        const events = [];
        const counts = { scans: 0, rawRows: 0, gets: 0, puts: 0, deletes: 0, creates: 0, begin: 0, commit: 0, rollback: 0 };
        const primary = new Map();
        const physical = new Map();
        const modes = new Map();
        const writes = [];
        let next = 1n;
        let failCommit = null;
        let needsRecovery = false;
        let recovered = false;
        let currentDocument = document;
        for (const row of rows) {
            physical.set(hex(row.physical), row.physical);
            primary.set(hex(row.key), row.value);
        }
        const sortedPhysical = Array.from(physical.values()).sort(cmp);
        function bound(key, inclusive) {
            let lo = 0, hi = sortedPhysical.length;
            while (lo < hi) {
                const mid = (lo + hi) >>> 1;
                const order = cmp(sortedPhysical[mid], key);
                if (order < 0 || (!inclusive && order === 0))
                    lo = mid + 1;
                else
                    hi = mid;
            }
            return lo;
        }
        const ranges = [];
        worker.dbName = 'test';
        worker.events = { postMessage(event) { events.push(event); } };
        worker.engine = {
            needs_recovery: () => needsRecovery,
            begin_tx(mode) { counts.begin++; const id = next++; modes.set(id, mode); return id; },
            commit_tx(id) {
                counts.commit++;
                modes.delete(id);
                if (failCommit) {
                    needsRecovery = recovered;
                    throw named(failCommit);
                }
                return id;
            },
            rollback_tx(id) { counts.rollback++; modes.delete(id); },
            recover() {
                needsRecovery = false;
                return { pendingTxid: next - 1n, pendingCommitted: true };
            },
            list_store_configs() { return Array.from(configs, ([name]) => ({ name, flags: 0 })); },
            create_store(id, name) {
                counts.creates++;
                if (modes.get(id) === 'readonly')
                    throw named('ReadonlyTransactionError');
                if (name === 'fail')
                    throw named('StoreExistsError');
            },
            drop_store(id) { if (modes.get(id) === 'readonly')
                throw named('ReadonlyTransactionError'); },
            clear_store() { },
            get(id, store, key) {
                counts.gets++;
                return rows.length ? primary.get(hex(key)) ?? null : currentDocument;
            },
            put(id, store, key, value) {
                counts.puts++;
                if (recordWrites)
                    writes.push(['put', store, hex(key), hex(value)]);
                if (store === 'docs') {
                    const existed = currentDocument !== null;
                    currentDocument = value;
                    return existed;
                }
                return false;
            },
            delete(id, store, key) {
                counts.deletes++;
                if (recordWrites)
                    writes.push(['delete', store, hex(key)]);
                if (store === 'docs') {
                    const existed = currentDocument !== null;
                    currentDocument = null;
                    return existed;
                }
                return physical.delete(hex(key));
            },
            scan(id, store, range) {
                if (range.limit === 0)
                    return [];
                counts.scans++;
                ranges.push(range);
                const lo = range.gt !== undefined ? bound(range.gt, false) :
                    range.gte !== undefined ? bound(range.gte, true) : 0;
                const hi = range.lt !== undefined ? bound(range.lt, true) :
                    range.lte !== undefined ? bound(range.lte, false) : sortedPhysical.length;
                const keys = [];
                const step = range.reverse ? -1 : 1;
                for (let i = range.reverse ? hi - 1 : lo; i >= lo && i < hi && keys.length < (range.limit ?? Infinity); i += step) {
                    const key = sortedPhysical[i];
                    if (physical.has(hex(key)))
                        keys.push(key);
                }
                counts.rawRows += keys.length;
                return keys.map((key) => ({ key: key.slice(), value: empty }));
            }
        };
        return {
            worker, configs, counts, ranges, writes, events, primary, physical,
            setCommitFailure(name, pendingCommitted = false) { failCommit = name; recovered = pendingCommitted; },
            setDocument(value) { currentDocument = value; },
            close() { worker.persistenceBridge.close(); }
        };
    }
    function definitions(count = 8, unique = false) {
        return indexing.normalizeIndexDefinitions(Array.from({ length: count }, (_, i) => ({
            store: 'docs', name: `i${i}`, keyPath: `k${i}`, unique
        })));
    }
    function document(count = 8, suffix = '', padding = 0) {
        return jsonEncode({ ...Object.fromEntries(Array.from({ length: count }, (_, i) => [`k${i}`, `v${i}${suffix}`])), padding: 'x'.repeat(padding) });
    }
    function indexRows(count, live = () => true, changed = () => false) {
        return Array.from({ length: count }, (_, i) => {
            const key = u64Key(i);
            return {
                key,
                physical: indexing.encodeIndexEntryKey(indexKey(i), key),
                value: live(i) ? jsonEncode({ k0: changed(i) ? -1 : i }) : null
            };
        });
    }
    async function counted(fn) {
        const savedMap = globalThis.Map;
        const savedParse = JSON.parse;
        const savedDecode = TextDecoder.prototype.decode;
        const counts = { mapEntriesCopied: 0, jsonParses: 0, utf8Decodes: 0 };
        globalThis.Map = class extends savedMap {
            constructor(iterable) {
                super();
                if (iterable !== undefined && iterable !== null) {
                    for (const [key, value] of iterable) {
                        counts.mapEntriesCopied++;
                        this.set(key, value);
                    }
                }
            }
        };
        JSON.parse = (...args) => { counts.jsonParses++; return savedParse(...args); };
        TextDecoder.prototype.decode = function (...args) { counts.utf8Decodes++; return savedDecode.apply(this, args); };
        try {
            return { result: await fn(), ...counts };
        }
        finally {
            globalThis.Map = savedMap;
            JSON.parse = savedParse;
            TextDecoder.prototype.decode = savedDecode;
        }
    }
    const tests = [];
    function test(name, fn, structural = false) { tests.push({ name, fn, structural }); }
    for (const mode of ['readonly', 'readwrite']) {
        test(`${mode} begin/end metadata work independent of catalog size`, async () => {
            for (const stores of [1, 100, 10000]) {
                const f = fixture({ stores });
                const c = await counted(async () => {
                    const id = await f.worker.begin(mode);
                    if (mode === 'readonly')
                        await f.worker.rollback(id);
                    else
                        await f.worker.commit(id);
                });
                eq(c.mapEntriesCopied, 0, `copied entries at ${stores} stores`);
                eq(f.worker.txStoreCompression.size, 0);
                f.close();
            }
        }, true);
    }
    test('create/drop detach once; old and concurrent readers keep their snapshots', async () => {
        const f = fixture({ stores: 100 });
        const oldReader = await f.worker.begin('readonly');
        const writer = await f.worker.begin('readwrite');
        const c = await counted(async () => {
            await f.worker.createStore(writer, 'created', { compression: 'gzip' });
            await f.worker.dropStore(writer, 's0');
            await f.worker.createStore(writer, 'second', { compression: 'gzip' });
        });
        eq(c.mapEntriesCopied, f.configs.size);
        const concurrent = await f.worker.begin('readonly');
        eq(f.worker.storeCompressionForTx(oldReader, 'created'), false);
        eq(f.worker.storeCompressionForTx(concurrent, 'second'), false);
        await f.worker.commit(writer);
        const newer = await f.worker.begin('readonly');
        eq(f.worker.storeCompressionForTx(newer, 'created'), 'gzip');
        eq(f.worker.storeCompressionForTx(newer, 'second'), 'gzip');
        eq(f.worker.storeCompressionForTx(oldReader, 'created'), false);
        // A subsequent writer must detach from the newly published map too.
        const secondWriter = await f.worker.begin('readwrite');
        await f.worker.dropStore(secondWriter, 'created');
        await f.worker.commit(secondWriter);
        eq(f.worker.storeCompressionForTx(newer, 'created'), 'gzip');
        eq(f.worker.committedStoreCompression.has('created'), false);
        for (const id of [oldReader, concurrent, newer])
            await f.worker.rollback(id);
        f.close();
    }, true);
    test('rollback discards schema edits; failed/readonly create cannot alter snapshots', async () => {
        const f = fixture();
        const id = await f.worker.begin('readwrite');
        await f.worker.createStore(id, 'new', { compression: 'gzip' });
        await f.worker.rollback(id);
        eq(f.worker.committedStoreCompression.has('new'), false);
        const ro = await f.worker.begin('readonly');
        await rejects(() => f.worker.createStore(ro, 'bad'), 'ReadonlyTransactionError');
        await f.worker.rollback(ro);
        const rw = await f.worker.begin('readwrite');
        await rejects(() => f.worker.createStore(rw, 'fail'), 'StoreExistsError');
        eq(f.worker.committedStoreCompression.has('fail'), false);
        await f.worker.rollback(rw);
        f.close();
    });
    for (const [failure, recovered, shouldSucceed] of [
        ['StorageError', false, false], ['StorageError', true, true], ['InjectedFailureError', true, false]
    ]) {
        test(`metadata publication on ${failure}, recovered=${recovered}`, async () => {
            const f = fixture();
            const id = await f.worker.begin('readwrite');
            await f.worker.createStore(id, 'new', { compression: 'gzip' });
            f.setCommitFailure(failure, recovered);
            if (shouldSucceed)
                await f.worker.commit(id);
            else
                await rejects(() => f.worker.commit(id), failure);
            eq(f.worker.txStoreCompression.size, 0);
            if (shouldSucceed)
                eq(f.worker.committedStoreCompression.get('new'), 'gzip');
            else if (!recovered)
                eq(f.worker.committedStoreCompression.has('new'), false);
            else
                eq(f.worker.committedStoreCompression, null);
            f.close();
        });
    }
    test('all transaction caches released on invalidation', async () => {
        const f = fixture();
        await f.worker.begin('readonly');
        await f.worker.begin('readwrite');
        f.worker.clearRuntimeCaches();
        eq(f.worker.txStoreCompression.size, 0);
        eq(f.worker.committedStoreCompression, null);
        f.close();
    });
    for (const indexes of [1, 8, 32]) {
        test(`indexed overwrite and delete decode once, ${indexes} definitions`, async () => {
            const f = fixture({ defs: definitions(indexes), document: document(indexes) });
            const tx = await f.worker.begin('readwrite');
            const put = await counted(() => f.worker.put(tx, 'docs', u64Key(1), document(indexes)));
            eq(put.jsonParses, 2);
            eq(put.utf8Decodes, 2);
            eq(f.writes.filter((w) => w[1] !== 'docs').length, 0, 'unchanged index key must not be rewritten');
            const del = await counted(() => f.worker.delete(tx, 'docs', u64Key(1)));
            eq(del.jsonParses, 1);
            eq(del.utf8Decodes, 1);
            eq(del.result, true);
            await f.worker.rollback(tx);
            f.close();
        }, true);
    }
    test('multi-index mutations preserve keys and change notifications', async () => {
        const defs = definitions(8);
        const oldDoc = document(8), newDoc = document(8, '-new');
        const f = fixture({ defs, document: oldDoc });
        const tx = await f.worker.begin('readwrite');
        const key = u64Key(7);
        await f.worker.put(tx, 'docs', key, newDoc);
        const expected = [['put', 'docs', hex(key), hex(newDoc)]];
        for (const def of defs)
            expected.push(['delete', def.internalStore, hex(indexing.encodeIndexEntryKey(indexing.extractLogicalIndexKey(def, oldDoc), key))]);
        for (const def of defs)
            expected.push(['put', def.internalStore, hex(indexing.encodeIndexEntryKey(indexing.extractLogicalIndexKey(def, newDoc), key)), '']);
        eq(f.writes, expected);
        await f.worker.commit(tx);
        eq(f.events[0].stores.map((s) => ({ store: s.store, kinds: s.changes.map((c) => c.kind) })), [{ store: 'docs', kinds: ['put'] }]);
        f.close();
    });
    test('operation-local decoding does not cache caller bytes across writes', async () => {
        const defs = definitions(2);
        const f = fixture({ defs, document: document(2) });
        const tx = await f.worker.begin('readwrite');
        const value = jsonEncode({ k0: 'a', k1: 'a' });
        await f.worker.put(tx, 'docs', u64Key(1), value);
        value.set(jsonEncode({ k0: 'b', k1: 'b' }));
        f.setDocument(document(2));
        await f.worker.put(tx, 'docs', u64Key(1), value);
        const last = f.writes.at(-1);
        eq(last[2], hex(indexing.encodeIndexEntryKey(indexKey('b'), u64Key(1))));
        f.close();
    });
    for (const [label, oldDoc, newDoc, fragment] of [
        ['old unsupported path before invalid new JSON', jsonEncode({ k0: [] }), new Uint8Array([255]), 'keyPath'],
        ['new first path before old second path', jsonEncode({ k0: 'ok', k1: [] }), jsonEncode({ k0: {} }), 'object'],
        ['bad UTF-8', document(2), new Uint8Array([255]), 'UTF-8'],
        ['bad JSON', document(2), new TextEncoder().encode('{'), 'JSON']
    ]) {
        test(`validation order: ${label}`, async () => {
            const f = fixture({ defs: definitions(2), document: oldDoc });
            const tx = await f.worker.begin('readwrite');
            await rejects(() => f.worker.put(tx, 'docs', u64Key(1), newDoc), 'SerializationError', fragment);
            eq(f.writes.length, 0);
            eq(f.counts.creates, 0);
            f.close();
        });
    }
    test('unchanged oversized physical key still rejected', async () => {
        const defs = definitions(1);
        const d = jsonEncode({ k0: 'x'.repeat(1020) });
        const f = fixture({ defs, document: d });
        const tx = await f.worker.begin('readwrite');
        await rejects(() => f.worker.put(tx, 'docs', u64Key(1), d), 'KeyTooLargeError');
        eq(f.writes.length, 0);
        f.close();
    });
    test('unique checks still reject a live conflicting primary row', async () => {
        const defs = definitions(1, true);
        const key = u64Key(7);
        const rows = [{ key, physical: indexing.encodeIndexEntryKey(indexKey('taken'), key), value: jsonEncode({ k0: 'taken' }) }];
        const f = fixture({ defs, rows });
        const tx = await f.worker.begin('readwrite');
        await rejects(() => f.worker.put(tx, 'docs', u64Key(8), jsonEncode({ k0: 'taken' })), 'UniqueIndexConstraintError');
        eq(f.writes.length, 0);
        f.close();
    });
    for (const reverse of [false, true]) {
        for (const limit of [1, 100, 256]) {
            test(`clean index scan reads only demand: reverse=${reverse}, limit=${limit}`, async () => {
                const f = fixture({ defs: definitions(1), rows: indexRows(2000) });
                const tx = await f.worker.begin('readonly');
                const got = await f.worker.scanByIndex(tx, 'docs', 'i0', { reverse, limit });
                eq(got.length, limit);
                eq(f.counts.rawRows, limit);
                eq(f.counts.gets, limit);
                const expected = Array.from({ length: limit }, (_, i) => hex(u64Key(reverse ? 1999 - i : i)));
                eq(got.map((r) => hex(r.key)), expected);
                f.close();
            }, true);
        }
        test(`bounded continuation with missing/stale rows, reverse=${reverse}`, async () => {
            const f = fixture({ defs: definitions(1), rows: indexRows(1500, (i) => i % 3 !== 0, (i) => i % 5 === 0) });
            const tx = await f.worker.begin('readwrite');
            const range = { gte: indexKey(100), lt: indexKey(1400), reverse };
            let cursor = null;
            const got = [];
            for (let n = 0; n < 100; n++) {
                const page = await f.worker.scanByIndexPage(tx, 'docs', 'i0', range, cursor, 37);
                got.push(...page.rows.map((r) => hex(r.key)));
                if (page.cursor === null)
                    break;
                cursor = page.cursor;
                ok(n !== 99, 'pagination did not terminate');
            }
            const expected = Array.from({ length: 1300 }, (_, i) => i + 100).filter((i) => i % 3 !== 0 && i % 5 !== 0);
            if (reverse)
                expected.reverse();
            eq(got, expected.map((i) => hex(u64Key(i))));
            eq(new Set(got).size, got.length);
            ok(f.counts.deletes > 0, 'stale cleanup did not execute');
            f.close();
        });
        test(`all stale prefix uses bounded geometric chunks, reverse=${reverse}`, async () => {
            const f = fixture({ defs: definitions(1), rows: indexRows(10000, () => false) });
            const tx = await f.worker.begin('readonly');
            const got = await f.worker.scanByIndex(tx, 'docs', 'i0', { reverse, limit: 1 });
            eq(got.length, 0);
            eq(f.counts.gets, 10000);
            eq(f.counts.deletes, 0);
            ok(f.counts.scans <= 30, `too many scans: ${f.counts.scans}`);
            ok(f.ranges.every((r) => r.limit >= 1 && r.limit <= 512));
            f.close();
        });
    }
    test('getByIndex reads one physical entry for a large duplicate-key range', async () => {
        const rows = Array.from({ length: 2000 }, (_, i) => ({
            key: u64Key(i),
            physical: indexing.encodeIndexEntryKey(indexKey(7), u64Key(i)),
            value: jsonEncode({ k0: 7, id: i })
        }));
        const f = fixture({ defs: definitions(1), rows });
        const tx = await f.worker.begin('readonly');
        const value = await f.worker.getByIndex(tx, 'docs', 'i0', indexKey(7));
        eq(bytes(value), bytes(rows[0].value));
        eq(f.counts.rawRows, 1);
        eq(f.counts.gets, 1);
        f.close();
    }, true);
    test('index error propagation and zero limits', async () => {
        const f = fixture({ defs: definitions(1) });
        const tx = await f.worker.begin('readonly');
        eq(await f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 0 }), []);
        await rejects(() => f.worker.scanByIndexPage(tx, 'docs', 'i0', {}, null, 0), 'InvalidRangeError');
        f.worker.engine.scan = () => { throw named('CorruptionError'); };
        await rejects(() => f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 1 }), 'CorruptionError');
        f.close();
    });
    test('lazy extractor preserves scalar, compound, missing and null behavior', async () => {
        const defs = indexing.normalizeIndexDefinitions([
            { store: 'docs', name: 'a', keyPath: 'name' },
            { store: 'docs', name: 'b', keyPath: ['name', 'nested.enabled'] },
            { store: 'docs', name: 'c', keyPath: 'absent' },
            { store: 'docs', name: 'd', keyPath: 'nested.enabled' }
        ]);
        for (const doc of [null, false, 42, 'text', [], { name: null, nested: { enabled: false } },
            { name: 'A\u0000Б', nested: { enabled: true } }, { name: 'partial' }]) {
            const encoded = jsonEncode(doc);
            const expected = defs.map((def) => bytes(indexing.extractLogicalIndexKey(def, encoded)));
            const c = await counted(() => {
                const extract = indexing.createIndexKeyExtractor(encoded);
                return defs.map((def) => bytes(extract(def)));
            });
            eq(c.result, expected);
            eq(c.jsonParses, 1);
            eq(c.utf8Decodes, 1);
        }
    }, true);
    test('insert decodes once; absent delete and unindexed bytes require no JSON', async () => {
        const f = fixture({ defs: definitions(8) });
        const tx = await f.worker.begin('readwrite');
        const inserted = await counted(() => f.worker.put(tx, 'docs', u64Key(1), document(8)));
        eq(inserted.jsonParses, 1);
        f.setDocument(null);
        const missing = await counted(() => f.worker.delete(tx, 'docs', u64Key(2)));
        eq(missing.result, false);
        eq(missing.jsonParses, 0);
        f.close();
        const raw = fixture();
        const c = await counted(() => raw.worker.autocommit('readwrite', 'put', ['docs', u64Key(1), new Uint8Array([255])]));
        eq(c.jsonParses, 0);
        eq(raw.counts.commit, 1);
        eq(raw.counts.rollback, 0);
        raw.close();
    }, true);
    test('autocommit validation failure rolls back and releases metadata', async () => {
        const f = fixture({ defs: definitions(8), document: document(8) });
        await rejects(() => f.worker.autocommit('readwrite', 'put', ['docs', u64Key(1), new Uint8Array([255])]), 'SerializationError');
        eq(f.counts.rollback, 1);
        eq(f.counts.commit, 0);
        eq(f.writes.length, 0);
        eq(f.worker.txStoreCompression.size, 0);
        f.close();
    });
    async function runTests(structural = true) {
        const results = [];
        for (const t of tests) {
            if (!structural && t.structural)
                continue;
            try {
                await t.fn();
                results.push({ name: t.name, passed: true });
            }
            catch (error) {
                results.push({ name: t.name, passed: false, error: String(error.stack ?? error) });
            }
        }
        return { passed: results.filter((r) => r.passed).length, failed: results.filter((r) => !r.passed).length, results };
    }
    async function measureWork() {
        const metadata = [];
        for (const stores of [1, 100, 10000]) {
            for (const mode of ['readonly', 'readwrite']) {
                const f = fixture({ stores });
                const c = await counted(async () => {
                    const tx = await f.worker.begin(mode);
                    if (mode === 'readonly')
                        await f.worker.rollback(tx);
                    else
                        await f.worker.commit(tx);
                });
                metadata.push({ stores: f.configs.size, mode, entriesCopied: c.mapEntriesCopied });
                f.close();
            }
        }
        const indexed = [];
        for (const indexes of [1, 8, 32]) {
            const f = fixture({ defs: definitions(indexes), document: document(indexes) });
            const tx = await f.worker.begin('readwrite');
            const put = await counted(() => f.worker.put(tx, 'docs', u64Key(1), document(indexes)));
            const del = await counted(() => f.worker.delete(tx, 'docs', u64Key(1)));
            indexed.push({ indexes, putParses: put.jsonParses, putDecodes: put.utf8Decodes, deleteParses: del.jsonParses });
            f.close();
        }
        const paging = [];
        for (const reverse of [false, true])
            for (const limit of [1, 100, 2000]) {
                const f = fixture({ defs: definitions(1), rows: indexRows(2000) });
                const tx = await f.worker.begin('readonly');
                await f.worker.scanByIndex(tx, 'docs', 'i0', { reverse, limit });
                paging.push({ reverse, limit, ...f.counts });
                f.close();
            }
        const f = fixture({ defs: definitions(1), rows: indexRows(10000, () => false) });
        const tx = await f.worker.begin('readonly');
        await f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 1 });
        const stale = { ...f.counts, limits: f.ranges.map((r) => r.limit) };
        f.close();
        return { metadata, indexed, paging, stale };
    }
    // Untimed fixture construction; no storage, wasm-bindgen, Worker messaging or
    // IndexedDB in these microbenchmarks. The caller controls warmups/order.
    function makeBenchmarks() {
        const metadata = fixture({ stores: 1000, recordWrites: false });
        const indexed = fixture({ defs: definitions(8), document: document(8, '', 65536), recordWrites: false });
        const d = document(8, '', 65536), key = u64Key(1);
        const scan = fixture({ defs: definitions(1), rows: indexRows(2000) });
        const small = fixture({ recordWrites: false });
        const stale = fixture({ defs: definitions(1), rows: indexRows(10000, () => false) });
        return {
            cases: {
                '1000 tiny autocommit writes / 2 configs': async () => {
                    for (let i = 0; i < 1000; i++)
                        await small.worker.autocommit('readwrite', 'put', ['docs', key, empty]);
                    small.events.length = 0;
                    small.writes.length = 0;
                },
                '10 all-stale index scans / limit=1 / 10000 rows': async () => {
                    const tx = await stale.worker.begin('readonly');
                    for (let i = 0; i < 10; i++)
                        await stale.worker.scanByIndex(tx, 'docs', 'i0', { limit: 1 });
                    await stale.worker.rollback(tx);
                    stale.ranges.length = 0;
                },
                '1000 tiny autocommit writes / 1001 configs': async () => {
                    for (let i = 0; i < 1000; i++)
                        await metadata.worker.autocommit('readwrite', 'put', ['docs', key, empty]);
                    metadata.events.length = 0;
                    metadata.writes.length = 0;
                },
                '100 indexed overwrites / 8 indexes / 64 KiB JSON': async () => {
                    const tx = await indexed.worker.begin('readwrite');
                    for (let i = 0; i < 100; i++)
                        await indexed.worker.put(tx, 'docs', key, d);
                    await indexed.worker.rollback(tx);
                    indexed.writes.length = 0;
                },
                '100 reverse index scans / limit=1 / 2000 rows': async () => {
                    const tx = await scan.worker.begin('readonly');
                    for (let i = 0; i < 100; i++)
                        await scan.worker.scanByIndex(tx, 'docs', 'i0', { reverse: true, limit: 1 });
                    await scan.worker.rollback(tx);
                    scan.ranges.length = 0;
                }
            },
            close() { metadata.close(); indexed.close(); scan.close(); small.close(); stale.close(); }
        };
    }
    return { runTests, measureWork, makeBenchmarks };
}
