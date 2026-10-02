// Runs in Node and Chromium. The WASM boundary is an ordered in-memory fixture;
// no assertions here imply disk durability, Rust work counts or OPFS latency.
export function createSuite({ runtime, indexing, codec }) {
    const { jsonEncode, indexKey, u64Key } = codec;
    const empty = new Uint8Array();
    const eq = (a, b, message = 'values differ') => {
        if (JSON.stringify(a) !== JSON.stringify(b))
            throw new Error(`${message}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`);
    };
    const ok = (condition, message = 'assertion failed') => {
        if (!condition) throw new Error(message);
    };
    const bytes = (v) => (v === null ? null : Array.from(v));
    const cmp = (a, b) => {
        for (let i = 0; i < Math.min(a.length, b.length); i++) if (a[i] !== b[i]) return a[i] - b[i];
        return a.length - b.length;
    };
    const hex = (v) => Array.from(v, (b) => b.toString(16).padStart(2, '0')).join('');
    const named = (name) => Object.assign(new Error(name), { name });
    async function rejects(fn, name, fragment) {
        try {
            await fn();
        } catch (error) {
            eq(error.name, name);
            if (fragment) ok(error.message.includes(fragment), error.message);
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
        const counts = {
            scans: 0,
            rawRows: 0,
            gets: 0,
            getCalls: 0,
            getManyCalls: 0,
            puts: 0,
            deletes: 0,
            creates: 0,
            begin: 0,
            commit: 0,
            rollback: 0
        };
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
            let lo = 0,
                hi = sortedPhysical.length;
            while (lo < hi) {
                const mid = (lo + hi) >>> 1;
                const order = cmp(sortedPhysical[mid], key);
                if (order < 0 || (!inclusive && order === 0)) lo = mid + 1;
                else hi = mid;
            }
            return lo;
        }
        const ranges = [];
        worker.dbName = 'test';
        worker.events = {
            postMessage(event) {
                events.push(event);
            }
        };
        worker.engine = {
            needs_recovery: () => needsRecovery,
            begin_tx(mode) {
                counts.begin++;
                const id = next++;
                modes.set(id, mode);
                return id;
            },
            commit_tx(id) {
                counts.commit++;
                modes.delete(id);
                if (failCommit) {
                    needsRecovery = recovered;
                    throw named(failCommit);
                }
                return id;
            },
            rollback_tx(id) {
                counts.rollback++;
                modes.delete(id);
            },
            recover() {
                needsRecovery = false;
                return { pendingTxid: next - 1n, pendingCommitted: true };
            },
            list_store_configs() {
                return Array.from(configs, ([name]) => ({ name, flags: 0 }));
            },
            create_store(id, name) {
                counts.creates++;
                if (modes.get(id) === 'readonly') throw named('ReadonlyTransactionError');
                if (name === 'fail') throw named('StoreExistsError');
            },
            drop_store(id) {
                if (modes.get(id) === 'readonly') throw named('ReadonlyTransactionError');
            },
            clear_store() {},
            get(id, store, key) {
                counts.getCalls++;
                counts.gets++;
                return rows.length ? (primary.get(hex(key)) ?? null) : currentDocument;
            },
            get_many(id, store, keys) {
                counts.getManyCalls++;
                counts.gets += keys.length;
                return keys.map((key) => (rows.length ? (primary.get(hex(key)) ?? null) : currentDocument));
            },
            put(id, store, key, value) {
                counts.puts++;
                if (recordWrites) writes.push(['put', store, hex(key), hex(value)]);
                if (store === 'docs') {
                    const existed = currentDocument !== null;
                    currentDocument = value;
                    return existed;
                }
                return false;
            },
            delete(id, store, key) {
                counts.deletes++;
                if (recordWrites) writes.push(['delete', store, hex(key)]);
                if (store === 'docs') {
                    const existed = currentDocument !== null;
                    currentDocument = null;
                    return existed;
                }
                return physical.delete(hex(key));
            },
            scan(id, store, range) {
                if (range.limit === 0) return [];
                counts.scans++;
                ranges.push(range);
                const lo =
                    range.gt !== undefined
                        ? bound(range.gt, false)
                        : range.gte !== undefined
                          ? bound(range.gte, true)
                          : 0;
                const hi =
                    range.lt !== undefined
                        ? bound(range.lt, true)
                        : range.lte !== undefined
                          ? bound(range.lte, false)
                          : sortedPhysical.length;
                const keys = [];
                const step = range.reverse ? -1 : 1;
                for (
                    let i = range.reverse ? hi - 1 : lo;
                    i >= lo && i < hi && keys.length < (range.limit ?? Infinity);
                    i += step
                ) {
                    const key = sortedPhysical[i];
                    if (physical.has(hex(key))) keys.push(key);
                }
                counts.rawRows += keys.length;
                return keys.map((key) => ({ key: key.slice(), value: empty }));
            }
        };
        return {
            worker,
            configs,
            counts,
            ranges,
            writes,
            events,
            primary,
            physical,
            setCommitFailure(name, pendingCommitted = false) {
                failCommit = name;
                recovered = pendingCommitted;
            },
            setDocument(value) {
                currentDocument = value;
            },
            close() {
                worker.persistenceBridge.close();
            }
        };
    }
    // Reconcile/lifecycle tests need separate stores and real transaction
    // snapshots. Values in this fixture are immutable, owned engine records.
    function catalogFixture({ defs = [], docs = [], internalRows = [], nativeVisibility = false } = {}) {
        const worker = new runtime.constructor();
        let committed = new Map([['docs', new Map()]]);
        let next = 1n;
        let now = 0;
        let beforeScan = () => {};
        let materialize = () => {};
        let beforeVisibility = () => {};
        let visibilityRead = () => {};
        const transactions = new Map();
        const counts = { creates: 0, getCalls: 0, getManyCalls: 0, hasManyCalls: 0, puts: 0, deletes: 0 };
        const scanCalls = new Map();
        const scanRows = new Map();
        const scanBytes = new Map();
        const writes = [];
        const copyStores = (stores) => new Map(Array.from(stores, ([name, rows]) => [name, new Map(rows)]));
        const record = (key, value, expiresAt = null, origin = 'base') => ({
            key: key.slice(),
            value: value.slice(),
            expiresAt,
            origin
        });
        for (const row of docs) committed.get('docs').set(hex(row.key), record(row.key, row.value, row.expiresAt));
        if (defs.length > 0) {
            const metadata = new Map();
            committed.set(indexing.INDEX_METADATA_STORE, metadata);
            for (const def of defs) {
                committed.set(def.internalStore, new Map());
                const key = indexing.encodeIndexMetadataKey(def.store, def.name);
                metadata.set(hex(key), record(key, indexing.encodeIndexMetadataValue(def)));
            }
        }
        for (const row of internalRows) {
            if (!committed.has(row.store)) committed.set(row.store, new Map());
            committed.get(row.store).set(hex(row.key), record(row.key, row.value ?? empty));
        }
        const txFor = (id, write = false) => {
            const tx = transactions.get(BigInt(id));
            if (!tx) throw named('TransactionClosedError');
            if (write && tx.mode !== 'readwrite') throw named('ReadonlyTransactionError');
            return tx;
        };
        const storeFor = (tx, store) => {
            const rows = tx.stores.get(store);
            if (!rows) throw named('StoreNotFoundError');
            return rows;
        };
        const visible = (tx, rows, row, operationNow = now) => {
            if (row.expiresAt !== null && row.expiresAt <= operationNow) {
                if (tx.mode === 'readwrite') rows.delete(hex(row.key));
                return false;
            }
            return true;
        };
        const normalizeStagedExpiry = (tx, rows, operationNow) => {
            if (tx.mode !== 'readwrite') return;
            for (const row of rows.values()) {
                if (row.origin === 'staged' && row.expiresAt !== null && row.expiresAt <= operationNow) {
                    rows.delete(hex(row.key));
                }
            }
        };
        const batchVisible = (tx, row, operationNow, expiredBaseKeys) => {
            if (row.expiresAt !== null && row.expiresAt <= operationNow) {
                if (tx.mode === 'readwrite' && row.origin === 'base') expiredBaseKeys.push(hex(row.key));
                return false;
            }
            return true;
        };
        worker.dbName = 'catalog-work';
        worker.events = { postMessage() {} };
        worker.committedIndexes = defs;
        worker.committedStoreCompression = new Map([['docs', false]]);
        worker.engine = {
            needs_recovery: () => false,
            begin_tx(mode) {
                const id = next++;
                transactions.set(id, { mode, stores: copyStores(committed) });
                return id;
            },
            commit_tx(id) {
                const tx = txFor(id);
                if (tx.mode === 'readwrite') {
                    for (const rows of tx.stores.values()) normalizeStagedExpiry(tx, rows, now);
                    committed = new Map(
                        Array.from(tx.stores, ([name, rows]) => [
                            name,
                            new Map(Array.from(rows, ([key, row]) => [key, { ...row, origin: 'base' }]))
                        ])
                    );
                }
                transactions.delete(id);
                return id;
            },
            rollback_tx(id) {
                txFor(id);
                transactions.delete(id);
            },
            list_store_configs() {
                return Array.from(committed, ([name]) => ({ name, flags: 0 }));
            },
            create_store(id, store) {
                counts.creates++;
                const tx = txFor(id, true);
                if (tx.stores.has(store)) throw named('StoreExistsError');
                tx.stores.set(store, new Map());
            },
            clear_store(id, store) {
                storeFor(txFor(id, true), store).clear();
            },
            drop_store(id, store) {
                const tx = txFor(id, true);
                storeFor(tx, store);
                tx.stores.delete(store);
            },
            get(id, store, key) {
                counts.getCalls++;
                const tx = txFor(id);
                const rows = storeFor(tx, store);
                const row = rows.get(hex(key));
                return row && visible(tx, rows, row) ? row.value.slice() : null;
            },
            get_many(id, store, keys) {
                counts.getManyCalls++;
                return keys.map((key) => this.get(id, store, key));
            },
            put(id, store, key, value, options = {}) {
                counts.puts++;
                const tx = txFor(id, true);
                const rows = storeFor(tx, store);
                const old = rows.get(hex(key));
                const existed = old !== undefined && visible(tx, rows, old);
                const expiresAt = options.ttl === undefined ? null : now + Number(options.ttl);
                rows.set(hex(key), record(key, value, expiresAt, 'staged'));
                writes.push(['put', store, hex(key)]);
                return existed;
            },
            delete(id, store, key) {
                counts.deletes++;
                const tx = txFor(id, true);
                const rows = storeFor(tx, store);
                const old = rows.get(hex(key));
                const existed = old !== undefined && visible(tx, rows, old);
                rows.delete(hex(key));
                writes.push(['delete', store, hex(key)]);
                return existed;
            },
            scan(id, store, range) {
                const tx = txFor(id);
                const rows = storeFor(tx, store);
                if (range.limit === 0) {
                    normalizeStagedExpiry(tx, rows, now);
                    return [];
                }
                scanCalls.set(store, (scanCalls.get(store) ?? 0) + 1);
                beforeScan(id, store, range);
                const operationNow = now;
                normalizeStagedExpiry(tx, rows, operationNow);
                const ordered = Array.from(rows.values()).sort((a, b) => cmp(a.key, b.key));
                if (range.reverse) ordered.reverse();
                const result = [];
                const expiredBaseKeys = [];
                for (const row of ordered) {
                    if (range.gt !== undefined && cmp(row.key, range.gt) <= 0) continue;
                    if (range.gte !== undefined && cmp(row.key, range.gte) < 0) continue;
                    if (range.lt !== undefined && cmp(row.key, range.lt) >= 0) continue;
                    if (range.lte !== undefined && cmp(row.key, range.lte) > 0) continue;
                    if (!batchVisible(tx, row, operationNow, expiredBaseKeys)) continue;
                    materialize(store, row);
                    result.push({ key: row.key.slice(), value: row.value.slice() });
                    scanRows.set(store, (scanRows.get(store) ?? 0) + 1);
                    scanBytes.set(store, (scanBytes.get(store) ?? 0) + row.key.byteLength + row.value.byteLength);
                    if (result.length >= (range.limit ?? Infinity)) break;
                }
                // Like the old full source scan, base TTL cleanup is applied
                // only when the complete issued read succeeds. Staged puts
                // were already normalized and remain deleted on later errors.
                for (const key of expiredBaseKeys) rows.delete(key);
                return result;
            }
        };
        if (nativeVisibility) {
            worker.engine.has_many = (id, store, keys) => {
                counts.hasManyCalls++;
                beforeVisibility(id, store, keys);
                const operationNow = now;
                const tx = txFor(id);
                const rows = storeFor(tx, store);
                normalizeStagedExpiry(tx, rows, operationNow);
                const expiredBaseKeys = [];
                const result = keys.map((key) => {
                    visibilityRead(id, store, key);
                    const row = rows.get(hex(key));
                    return row !== undefined && batchVisible(tx, row, operationNow, expiredBaseKeys);
                });
                for (const key of expiredBaseKeys) rows.delete(key);
                return result;
            };
        }
        return {
            worker,
            counts,
            scanCalls,
            scanRows,
            scanBytes,
            writes,
            rows(txId, store) {
                const stores = txId === null ? committed : txFor(txId).stores;
                return Array.from(stores.get(store)?.values() ?? [], (row) => ({
                    key: row.key.slice(),
                    value: row.value.slice(),
                    expiresAt: row.expiresAt,
                    origin: row.origin
                }));
            },
            setNow(value) {
                now = value;
            },
            beforeScan(fn) {
                beforeScan = fn;
            },
            materialize(fn) {
                materialize = fn;
            },
            beforeVisibility(fn) {
                beforeVisibility = fn;
            },
            visibilityRead(fn) {
                visibilityRead = fn;
            },
            close() {
                worker.persistenceBridge.close();
            }
        };
    }
    function definitions(count = 8, unique = false) {
        return indexing.normalizeIndexDefinitions(
            Array.from({ length: count }, (_, i) => ({
                store: 'docs',
                name: `i${i}`,
                keyPath: `k${i}`,
                unique
            }))
        );
    }
    function document(count = 8, suffix = '', padding = 0) {
        return jsonEncode({
            ...Object.fromEntries(Array.from({ length: count }, (_, i) => [`k${i}`, `v${i}${suffix}`])),
            padding: 'x'.repeat(padding)
        });
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
        JSON.parse = (...args) => {
            counts.jsonParses++;
            return savedParse(...args);
        };
        TextDecoder.prototype.decode = function (...args) {
            counts.utf8Decodes++;
            return savedDecode.apply(this, args);
        };
        try {
            return { result: await fn(), ...counts };
        } finally {
            globalThis.Map = savedMap;
            JSON.parse = savedParse;
            TextDecoder.prototype.decode = savedDecode;
        }
    }
    const tests = [];
    function test(name, fn, structural = false) {
        tests.push({ name, fn, structural });
    }
    for (const mode of ['readonly', 'readwrite']) {
        test(
            `${mode} begin/end metadata work independent of catalog size`,
            async () => {
                for (const stores of [1, 100, 10000]) {
                    const f = fixture({ stores });
                    const c = await counted(async () => {
                        const id = await f.worker.begin(mode);
                        if (mode === 'readonly') await f.worker.rollback(id);
                        else await f.worker.commit(id);
                    });
                    eq(c.mapEntriesCopied, 0, `copied entries at ${stores} stores`);
                    eq(f.worker.txStoreCompression.size, 0);
                    f.close();
                }
            },
            true
        );
    }
    test(
        'create/drop detach once; old and concurrent readers keep their snapshots',
        async () => {
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
            for (const id of [oldReader, concurrent, newer]) await f.worker.rollback(id);
            f.close();
        },
        true
    );
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
        ['StorageError', false, false],
        ['StorageError', true, true],
        ['InjectedFailureError', true, false]
    ]) {
        test(`metadata publication on ${failure}, recovered=${recovered}`, async () => {
            const f = fixture();
            const id = await f.worker.begin('readwrite');
            await f.worker.createStore(id, 'new', { compression: 'gzip' });
            f.setCommitFailure(failure, recovered);
            if (shouldSucceed) await f.worker.commit(id);
            else await rejects(() => f.worker.commit(id), failure);
            eq(f.worker.txStoreCompression.size, 0);
            if (shouldSucceed) eq(f.worker.committedStoreCompression.get('new'), 'gzip');
            else if (!recovered) eq(f.worker.committedStoreCompression.has('new'), false);
            else eq(f.worker.committedStoreCompression, null);
            f.close();
        });
    }
    test('all transaction caches released on invalidation', async () => {
        const f = fixture({ defs: definitions(1), document: document(1) });
        await f.worker.begin('readonly');
        const writer = await f.worker.begin('readwrite');
        await f.worker.put(writer, 'docs', u64Key(1), document(1));
        eq(f.worker.txEnsuredRawStores.size, 1);
        f.worker.clearRuntimeCaches();
        eq(f.worker.txStoreCompression.size, 0);
        eq(f.worker.txEnsuredRawStores.size, 0);
        eq(f.worker.committedStoreCompression, null);
        f.close();
    });
    test('internal store confirmation is scoped to transaction and lifecycle', async () => {
        const defs = definitions(8);
        const value = document(8);
        const key = u64Key(1);
        const f = catalogFixture({
            defs,
            docs: [{ key, value }],
            internalRows: defs.map((def) => ({
                store: def.internalStore,
                key: indexing.encodeIndexEntryKey(indexing.extractLogicalIndexKey(def, value), key)
            }))
        });
        try {
            const writer = await f.worker.begin('readwrite');
            try {
                for (let index = 0; index < 3; index++) await f.worker.put(writer, 'docs', key, value);
                eq(f.counts.creates, 8);
                await f.worker.clearStore(writer, 'docs');
                eq(f.rows(writer, 'docs').length, 0);
                await f.worker.put(writer, 'docs', key, value);
                for (const def of defs) eq(f.rows(writer, def.internalStore).length, 1);
                eq(f.counts.creates, 8, 'clear preserves confirmed store existence');
            } finally {
                await f.worker.rollback(writer);
            }
            eq(f.worker.txEnsuredRawStores.size, 0);
            eq(bytes(f.rows(null, 'docs')[0].value), bytes(value));
            const next = await f.worker.begin('readwrite');
            try {
                await f.worker.put(next, 'docs', key, value);
                eq(f.counts.creates, 16);
            } finally {
                await f.worker.rollback(next);
            }
            const reader = await f.worker.begin('readonly');
            try {
                await rejects(() => f.worker.put(reader, 'docs', key, value), 'ReadonlyTransactionError');
                eq(f.worker.txEnsuredRawStores.size, 0);
            } finally {
                await f.worker.rollback(reader);
            }
        } finally {
            f.close();
        }
    });
    test('failed internal store confirmation is retried and existing stores are remembered', async () => {
        const f = fixture({ defs: definitions(1), document: document(1) });
        const writer = await f.worker.begin('readwrite');
        let attempts = 0;
        f.worker.engine.create_store = () => {
            attempts++;
            throw named(attempts === 1 ? 'StorageError' : 'StoreExistsError');
        };
        await rejects(() => f.worker.put(writer, 'docs', u64Key(1), document(1)), 'StorageError');
        eq(f.counts.puts, 0);
        await f.worker.put(writer, 'docs', u64Key(1), document(1));
        await f.worker.put(writer, 'docs', u64Key(1), document(1));
        eq(attempts, 2);
        await f.worker.rollback(writer);
        f.close();
    });
    for (const indexes of [1, 8, 32]) {
        test(
            `indexed overwrite and delete decode once, ${indexes} definitions`,
            async () => {
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
            },
            true
        );
    }
    test('multi-index mutations preserve keys and change notifications', async () => {
        const defs = definitions(8);
        const oldDoc = document(8),
            newDoc = document(8, '-new');
        const f = fixture({ defs, document: oldDoc });
        const tx = await f.worker.begin('readwrite');
        const key = u64Key(7);
        await f.worker.put(tx, 'docs', key, newDoc);
        const expected = [['put', 'docs', hex(key), hex(newDoc)]];
        for (const def of defs)
            expected.push([
                'delete',
                def.internalStore,
                hex(indexing.encodeIndexEntryKey(indexing.extractLogicalIndexKey(def, oldDoc), key))
            ]);
        for (const def of defs)
            expected.push([
                'put',
                def.internalStore,
                hex(indexing.encodeIndexEntryKey(indexing.extractLogicalIndexKey(def, newDoc), key)),
                ''
            ]);
        eq(f.writes, expected);
        await f.worker.commit(tx);
        eq(
            f.events[0].stores.map((s) => ({ store: s.store, kinds: s.changes.map((c) => c.kind) })),
            [{ store: 'docs', kinds: ['put'] }]
        );
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
        const rows = [
            { key, physical: indexing.encodeIndexEntryKey(indexKey('taken'), key), value: jsonEncode({ k0: 'taken' }) }
        ];
        const f = fixture({ defs, rows });
        const tx = await f.worker.begin('readwrite');
        await rejects(
            () => f.worker.put(tx, 'docs', u64Key(8), jsonEncode({ k0: 'taken' })),
            'UniqueIndexConstraintError'
        );
        eq(f.writes.length, 0);
        f.close();
    });
    test('failed clear preserves confirmation; missing-store clear removes it', async () => {
        const defs = definitions(1);
        const key = u64Key(1);
        const value = document(1);
        const f = catalogFixture({ defs, docs: [{ key, value }] });
        const writer = await f.worker.begin('readwrite');
        try {
            await f.worker.put(writer, 'docs', key, value);
            const clear = f.worker.engine.clear_store;
            f.worker.engine.clear_store = (id, store) => {
                if (store === defs[0].internalStore) throw named('StorageError');
                return clear(id, store);
            };
            await rejects(() => f.worker.clearStore(writer, 'docs'), 'StorageError');
            await f.worker.put(writer, 'docs', key, value);
            eq(f.counts.creates, 1, 'a failed clear did not drop the store');
            f.worker.engine.clear_store = clear;
            f.worker.engine.drop_store(BigInt(writer), defs[0].internalStore);
            f.worker.clearRawStoreIfExists(writer, defs[0].internalStore);
            eq(f.worker.txEnsuredRawStores.get(writer).has(defs[0].internalStore), false);
            await f.worker.put(writer, 'docs', key, document(1, '-changed'));
            eq(f.counts.creates, 2);
            eq(f.rows(writer, defs[0].internalStore).length, 1);
        } finally {
            await f.worker.rollback(writer);
            eq(bytes(f.rows(null, 'docs')[0].value), bytes(value));
            f.close();
        }
    });
    test(
        'unique rejection reads only the first conflicting physical row',
        async () => {
            const defs = definitions(1, true);
            const docs = Array.from({ length: 32 }, (_, i) => ({ key: u64Key(i), value: jsonEncode({ k0: 'taken' }) }));
            // A corrupt document at the tail cannot precede the first conflict.
            docs[31].value = new Uint8Array([255]);
            const f = catalogFixture({
                defs,
                docs,
                internalRows: docs.map((row) => ({
                    store: defs[0].internalStore,
                    key: indexing.encodeIndexEntryKey(indexKey('taken'), row.key)
                }))
            });
            const writer = await f.worker.begin('readwrite');
            try {
                await rejects(
                    () => f.worker.put(writer, 'docs', u64Key(32), jsonEncode({ k0: 'taken' })),
                    'UniqueIndexConstraintError'
                );
                eq(f.scanRows.get(defs[0].internalStore), 1, 'unique lookup materialized the unused tail');
                eq(f.counts.getCalls, 2);
                eq(f.counts.getManyCalls, 0);
                eq(f.writes, []);
            } finally {
                await f.worker.rollback(writer);
                eq(f.rows(null, defs[0].internalStore).length, 32);
                f.close();
            }
        },
        true
    );
    test(
        'unique first conflict precedes an unreachable raw-materialization fault',
        async () => {
            const defs = definitions(1, true);
            const firstKey = u64Key(0);
            const tailKey = u64Key(31);
            const tailPhysical = indexing.encodeIndexEntryKey(indexKey('taken'), tailKey);
            const f = catalogFixture({
                defs,
                docs: [{ key: firstKey, value: jsonEncode({ k0: 'taken' }) }],
                internalRows: Array.from({ length: 32 }, (_, i) => ({
                    store: defs[0].internalStore,
                    key: indexing.encodeIndexEntryKey(indexKey('taken'), u64Key(i))
                }))
            });
            let tailFaults = 0;
            f.materialize((store, row) => {
                if (store === defs[0].internalStore && hex(row.key) === hex(tailPhysical)) {
                    tailFaults++;
                    throw named('CorruptionError');
                }
            });
            const writer = await f.worker.begin('readwrite');
            try {
                await rejects(
                    () => f.worker.put(writer, 'docs', u64Key(32), jsonEncode({ k0: 'taken' })),
                    'UniqueIndexConstraintError'
                );
                eq(tailFaults, 0, 'unique rejection must stop before unrelated raw corruption');
                eq(f.scanRows.get(defs[0].internalStore), 1);
                eq(f.writes, []);
            } finally {
                await f.worker.rollback(writer);
                eq(f.rows(null, defs[0].internalStore).length, 32);
                f.close();
            }
        },
        true
    );
    test('unique lookup preserves stale cleanup and document error order before rollback', async () => {
        const defs = definitions(1, true);
        const docs = [
            { key: u64Key(0), value: jsonEncode({ k0: 'taken' }), expiresAt: 5 },
            { key: u64Key(1), value: new Uint8Array([255]) },
            { key: u64Key(2), value: jsonEncode({ k0: 'taken' }) }
        ];
        const f = catalogFixture({
            defs,
            docs,
            internalRows: docs.map((row) => ({
                store: defs[0].internalStore,
                key: indexing.encodeIndexEntryKey(indexKey('taken'), row.key)
            }))
        });
        f.setNow(5);
        const writer = await f.worker.begin('readwrite');
        try {
            await rejects(
                () => f.worker.put(writer, 'docs', u64Key(3), jsonEncode({ k0: 'taken' })),
                'SerializationError',
                'UTF-8'
            );
            eq(f.counts.getManyCalls, 0);
            eq(f.rows(writer, 'docs').length, 2);
            eq(f.rows(writer, defs[0].internalStore).length, 2);
            eq(
                f.writes.map((write) => write.slice(0, 2)),
                [['delete', defs[0].internalStore]]
            );
        } finally {
            await f.worker.rollback(writer);
            eq(f.rows(null, 'docs').length, 3);
            eq(f.rows(null, defs[0].internalStore).length, 3);
            f.close();
        }
    });
    test('unique lookup walks a stale prefix and keeps its own primary entry', async () => {
        const defs = definitions(1, true);
        const ownKey = u64Key(31);
        const docs = Array.from({ length: 32 }, (_, i) => ({
            key: u64Key(i),
            value: jsonEncode({ k0: i === 31 ? 'prior' : 'taken' }),
            expiresAt: i === 31 ? null : 5
        }));
        const f = catalogFixture({
            defs,
            docs,
            internalRows: docs.map((row) => ({
                store: defs[0].internalStore,
                key: indexing.encodeIndexEntryKey(indexKey('taken'), row.key)
            }))
        });
        f.setNow(5);
        const writer = await f.worker.begin('readwrite');
        try {
            await f.worker.put(writer, 'docs', ownKey, jsonEncode({ k0: 'taken' }));
            eq(f.counts.getManyCalls, 0);
            eq(f.counts.getCalls, 32, 'own primary key must be skipped in the unique check');
            eq(f.rows(writer, 'docs').length, 1);
            eq(
                f.rows(writer, defs[0].internalStore).map((row) => hex(row.key)),
                [hex(indexing.encodeIndexEntryKey(indexKey('taken'), ownKey))]
            );
        } finally {
            await f.worker.rollback(writer);
            eq(f.rows(null, 'docs').length, 32);
            eq(f.rows(null, defs[0].internalStore).length, 32);
            f.close();
        }
    });
    for (const indexes of [1, 8]) {
        test(
            `reconcile reuses document decoding but refreshes source visibility, ${indexes} indexes`,
            async () => {
                const defs = definitions(indexes);
                const docs = Array.from({ length: 32 }, (_, i) => ({
                    key: u64Key(i),
                    value: document(indexes, `-${i}`)
                }));
                const f = catalogFixture({ docs });
                const writer = await f.worker.begin('readwrite');
                try {
                    const c = await counted(() =>
                        f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs))
                    );
                    eq(c.jsonParses, 32, 'unchanged owned documents were parsed again for another index');
                    eq(c.utf8Decodes, 32);
                    eq(f.scanCalls.get('docs'), indexes, 'every index must observe fresh TTL visibility');
                    eq(f.scanRows.get('docs'), 32 * indexes);
                    for (const def of defs) eq(f.rows(writer, def.internalStore).length, 32);
                    eq(f.rows(writer, indexing.INDEX_METADATA_STORE).length, indexes);
                } finally {
                    await f.worker.rollback(writer);
                    eq(f.rows(null, 'docs').length, 32);
                    eq(f.rows(null, indexing.INDEX_METADATA_STORE), []);
                    for (const def of defs) eq(f.rows(null, def.internalStore), []);
                    eq(f.worker.committedIndexes, []);
                    f.close();
                }
            },
            true
        );
    }
    test(
        'native reconcile reads source payload once and refreshes visibility for each index',
        async () => {
            const defs = definitions(8);
            const docs = Array.from({ length: 32 }, (_, i) => ({ key: u64Key(i), value: document(8, `-${i}`) }));
            const f = catalogFixture({ docs, nativeVisibility: true });
            const writer = await f.worker.begin('readwrite');
            try {
                const c = await counted(() =>
                    f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs))
                );
                eq(f.scanRows.get('docs'), 32, 'unchanged source payload was materialized for each index');
                eq(f.scanCalls.get('docs'), 1);
                eq(f.counts.hasManyCalls, 7, 'each later index needs one fresh native visibility observation');
                eq(c.jsonParses, 32);
                eq(c.utf8Decodes, 32);
                for (const def of defs) eq(f.rows(writer, def.internalStore).length, 32);
            } finally {
                await f.worker.rollback(writer);
                eq(f.rows(null, indexing.INDEX_METADATA_STORE), []);
                f.close();
            }
        },
        true
    );
    test('native reconcile above the byte budget retains fresh source scans', async () => {
        const defs = definitions(2);
        const value = jsonEncode({ k0: 'A', k1: 'B', padding: 'x'.repeat(2 * 1024 * 1024) });
        const f = catalogFixture({
            docs: [
                { key: u64Key(0), value },
                { key: u64Key(1), value }
            ],
            nativeVisibility: true
        });
        const writer = await f.worker.begin('readwrite');
        try {
            const c = await counted(() => f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs)));
            eq(f.scanCalls.get('docs'), 2);
            eq(f.scanRows.get('docs'), 4);
            eq(f.counts.hasManyCalls, 0, 'oversized sources must not be retained');
            eq(c.jsonParses, 3, 'the JSON input budget can retain only one of these documents');
            for (const def of defs) eq(f.rows(writer, def.internalStore).length, 2);
        } finally {
            await f.worker.rollback(writer);
            eq(f.rows(null, indexing.INDEX_METADATA_STORE), []);
            f.close();
        }
    });
    test('native reconcile preserves an initial scan fault and base/staged TTL distinction', async () => {
        const defs = definitions(2);
        const docs = Array.from({ length: 32 }, (_, i) => ({
            key: u64Key(i),
            value: document(2, `-${i}`),
            expiresAt: i === 0 ? 5 : null
        }));
        const f = catalogFixture({ docs, nativeVisibility: true });
        f.materialize((store, row) => {
            if (store === 'docs' && hex(row.key) === hex(u64Key(31))) throw named('CorruptionError');
        });
        const writer = await f.worker.begin('readwrite');
        try {
            await f.worker.put(writer, 'docs', u64Key(1), docs[1].value, { ttl: 5 });
            f.setNow(5);
            await rejects(
                () => f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs)),
                'CorruptionError'
            );
            eq(f.counts.hasManyCalls, 0);
            eq(f.rows(writer, 'docs').length, 31);
            const remaining = f.rows(writer, 'docs');
            eq(remaining.find((row) => hex(row.key) === hex(u64Key(0))).origin, 'base');
            eq(
                remaining.some((row) => hex(row.key) === hex(u64Key(1))),
                false
            );
            eq(f.worker.txIndexSchemaChanged.has(writer), false);
        } finally {
            await f.worker.rollback(writer);
            eq(f.rows(null, 'docs').length, 32);
            eq(f.rows(null, indexing.INDEX_METADATA_STORE), []);
            f.close();
        }
    });
    test('native reconcile matches the old scan TTL state after a later read error', async () => {
        const defs = definitions(2);
        const docs = Array.from({ length: 32 }, (_, i) => ({
            key: u64Key(i),
            value: i === 0 ? jsonEncode({ k0: 'valid', k1: [] }) : document(2, `-${i}`),
            expiresAt: i === 1 ? 5 : null
        }));
        const states = [];
        for (const nativeVisibility of [false, true]) {
            const f = catalogFixture({ docs, nativeVisibility });
            f.beforeScan((id, store) => {
                if (store === 'docs' && f.scanCalls.get(store) === 2) f.setNow(5);
            });
            f.materialize((store, row) => {
                if (store === 'docs' && f.scanCalls.get(store) === 2 && hex(row.key) === hex(u64Key(31))) {
                    throw named('CorruptionError');
                }
            });
            f.beforeVisibility(() => f.setNow(5));
            f.visibilityRead((id, store, key) => {
                if (hex(key) === hex(u64Key(31))) throw named('CorruptionError');
            });
            const writer = await f.worker.begin('readwrite');
            try {
                await f.worker.put(writer, 'docs', u64Key(2), docs[2].value, { ttl: 5 });
                eq(f.rows(writer, 'docs').find((row) => hex(row.key) === hex(u64Key(2))).origin, 'staged');
                await rejects(
                    () => f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs)),
                    'CorruptionError'
                );
                eq(f.counts.hasManyCalls, nativeVisibility ? 1 : 0);
                const remaining = f.rows(writer, 'docs');
                eq(remaining.length, 31);
                eq(remaining.find((row) => hex(row.key) === hex(u64Key(1))).origin, 'base');
                eq(
                    remaining.some((row) => hex(row.key) === hex(u64Key(2))),
                    false
                );
                states.push(remaining.map((row) => [hex(row.key), hex(row.value), row.expiresAt, row.origin]));
                eq(f.rows(writer, defs[0].internalStore).length, 32);
                eq(f.rows(writer, defs[1].internalStore), []);
                eq(f.worker.txIndexSchemaChanged.has(writer), false);
            } finally {
                await f.worker.rollback(writer);
                eq(f.rows(null, 'docs').length, 32);
                eq(
                    f.rows(null, 'docs').every((row) => row.origin === 'base'),
                    true
                );
                eq(f.rows(null, indexing.INDEX_METADATA_STORE), []);
                f.close();
            }
        }
        eq(states[1], states[0], 'native visibility must preserve the legacy scan partial-TTL state');
    });
    test('native reconcile keeps owned source values when caller input changes', async () => {
        const defs = definitions(2);
        const key = u64Key(0);
        const value = document(2);
        const original = value.slice();
        const f = catalogFixture({ docs: [{ key, value }], nativeVisibility: true });
        f.beforeVisibility(() => {
            value[0] = 255;
        });
        const writer = await f.worker.begin('readwrite');
        try {
            await f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs));
            eq(f.counts.hasManyCalls, 1);
            eq(value[0], 255);
            eq(bytes(f.rows(writer, 'docs')[0].value), bytes(original));
            eq(
                f
                    .rows(writer, defs[1].internalStore)
                    .map((row) => hex(indexing.decodeIndexEntryKey(row.key).logicalKey)),
                [hex(indexKey('v1'))]
            );
        } finally {
            await f.worker.rollback(writer);
            eq(bytes(f.rows(null, 'docs')[0].value), bytes(original));
            f.close();
        }
    });
    test('native reconcile rejects a visibility outcome count mismatch', async () => {
        const defs = definitions(2);
        const f = catalogFixture({ docs: [{ key: u64Key(0), value: document(2) }], nativeVisibility: true });
        f.worker.engine.has_many = () => [];
        const writer = await f.worker.begin('readwrite');
        try {
            await rejects(
                () => f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs)),
                'InternalError',
                'outcome count'
            );
        } finally {
            await f.worker.rollback(writer);
            eq(f.rows(null, indexing.INDEX_METADATA_STORE), []);
            f.close();
        }
    });
    for (const nativeVisibility of [false, true]) {
        test(`reconcile observes expiry between indexes and rollback restores the source, native=${nativeVisibility}`, async () => {
            const defs = definitions(2);
            const docs = [
                { key: u64Key(0), value: document(2), expiresAt: 5 },
                { key: u64Key(1), value: document(2, '-live') }
            ];
            const f = catalogFixture({ docs, nativeVisibility });
            f.beforeScan((id, store) => {
                if (store === 'docs' && f.scanCalls.get(store) === 2) f.setNow(5);
            });
            f.beforeVisibility(() => f.setNow(5));
            const writer = await f.worker.begin('readwrite');
            try {
                await f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs));
                eq(f.scanCalls.get('docs'), nativeVisibility ? 1 : 2);
                eq(f.rows(writer, 'docs').length, 1);
                eq(f.rows(writer, defs[0].internalStore).length, 2);
                eq(
                    f
                        .rows(writer, defs[1].internalStore)
                        .map((row) => hex(indexing.decodeIndexEntryKey(row.key).primaryKey)),
                    [hex(u64Key(1))]
                );
            } finally {
                await f.worker.rollback(writer);
                eq(f.rows(null, 'docs').length, 2);
                eq(f.rows(null, indexing.INDEX_METADATA_STORE), []);
                f.close();
            }
        });
    }
    test('reconcile cannot reuse parsed JSON when a source row version changes', async () => {
        const defs = definitions(2);
        const key = u64Key(0);
        const oldValue = document(2);
        const newValue = jsonEncode({ k0: 'v0', k1: 'new' });
        const f = catalogFixture({ docs: [{ key, value: oldValue }] });
        f.beforeScan((id, store) => {
            if (store === 'docs' && f.scanCalls.get(store) === 2) f.worker.engine.put(id, store, key, newValue);
        });
        const writer = await f.worker.begin('readwrite');
        try {
            const c = await counted(() => f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs)));
            eq(c.jsonParses, 2);
            eq(
                f
                    .rows(writer, defs[1].internalStore)
                    .map((row) => hex(indexing.decodeIndexEntryKey(row.key).logicalKey)),
                [hex(indexKey('new'))]
            );
        } finally {
            await f.worker.rollback(writer);
            eq(bytes(f.rows(null, 'docs')[0].value), bytes(oldValue));
            f.close();
        }
    });
    test('reconcile validates in index then row order and rolls back a partial index', async () => {
        const defs = definitions(2);
        const f = catalogFixture({
            docs: [
                { key: u64Key(0), value: jsonEncode({ k0: 'valid', k1: [] }) },
                { key: u64Key(1), value: jsonEncode({ k0: {}, k1: 'valid' }) }
            ]
        });
        const writer = await f.worker.begin('readwrite');
        try {
            await rejects(
                () => f.worker.reconcileIndexes(writer, indexing.toPublicIndexDefinitions(defs)),
                'SerializationError',
                'object'
            );
            eq(f.rows(writer, defs[0].internalStore).length, 1);
            eq(f.rows(writer, indexing.INDEX_METADATA_STORE), []);
            eq(f.worker.txIndexSchemaChanged.has(writer), false);
        } finally {
            await f.worker.rollback(writer);
            eq(f.rows(null, defs[0].internalStore), []);
            eq(f.rows(null, 'docs').length, 2);
            eq(f.worker.committedIndexes, []);
            f.close();
        }
    });
    for (const operation of ['compact', 'rebuild']) {
        test(
            `generation ${operation} copies only the stores it will preserve`,
            async () => {
                const worker = new runtime.constructor();
                const defs = definitions(1);
                const unknownInternal = '__browserdb:other-feature';
                const sourceStores = new Map([
                    ['docs', 'source'],
                    [unknownInternal, 'opaque'],
                    [defs[0].internalStore, 'old-index'],
                    [indexing.INDEX_METADATA_STORE, 'old-metadata']
                ]);
                const calls = [];
                let active = 'old';
                const source = {
                    needs_recovery: () => false,
                    abandon() {},
                    compact_into(target) {
                        calls.push({ method: 'compact_into', skipped: [] });
                        target.stores = new Map(sourceStores);
                        return 1n;
                    },
                    compact_into_skipping_stores(target, skipped) {
                        calls.push({ method: 'compact_into_skipping_stores', skipped });
                        target.stores = new Map(Array.from(sourceStores).filter(([store]) => !skipped.includes(store)));
                        return 1n;
                    }
                };
                worker.engine = source;
                worker.dbName = 'rebuild-work';
                worker.committedIndexes = defs;
                worker.committedStoreCompression = new Map([['docs', false]]);
                worker.loadWasm = async () => ({
                    WasmEngine: class {
                        stores = new Map();
                        async openGeneration() {}
                        abandon() {}
                    },
                    dbDirectorySize: async () => 100,
                    readActiveGeneration: async () => active,
                    prepareRebuildTarget: async () => ({ generationName: 'new' }),
                    swapActiveGeneration: async () => {
                        active = 'new';
                    },
                    cleanupInactiveEntries: async () => {}
                });
                let beforeRegeneration = null;
                worker.applyCommittedIndexSchema = async (indexes) => {
                    beforeRegeneration = Array.from(worker.engine.stores.keys()).sort();
                    for (const def of indexes) worker.engine.stores.set(def.internalStore, 'fresh-index');
                    worker.engine.stores.set(indexing.INDEX_METADATA_STORE, 'fresh-metadata');
                };
                try {
                    await worker[operation]();
                    if (operation === 'rebuild') {
                        eq(calls, [
                            {
                                method: 'compact_into_skipping_stores',
                                skipped: [defs[0].internalStore, indexing.INDEX_METADATA_STORE]
                            }
                        ]);
                        eq(beforeRegeneration, ['docs', unknownInternal].sort());
                        eq(worker.engine.stores.get(defs[0].internalStore), 'fresh-index');
                    } else {
                        eq(calls, [{ method: 'compact_into', skipped: [] }]);
                        eq(beforeRegeneration, null);
                        eq(worker.engine.stores.get(defs[0].internalStore), 'old-index');
                    }
                    eq(worker.engine.stores.get(unknownInternal), 'opaque');
                    eq(worker.engine.stores.get('docs'), 'source');
                    eq(active, 'new');
                } finally {
                    worker.persistenceBridge.close();
                }
            },
            operation === 'rebuild'
        );
    }
    for (const mode of ['readonly', 'readwrite']) {
        test(
            `indexed row keys are independently owned without a second copy, mode=${mode}`,
            async () => {
                const rows = Array.from({ length: 2 }, (_, id) => {
                    const key = new Uint8Array(257).fill(0x41 + id);
                    return {
                        key,
                        physical: indexing.encodeIndexEntryKey(indexKey(id), key),
                        value: jsonEncode({ k0: id, id })
                    };
                });
                const expectedKeys = rows.map((row) => bytes(row.key));
                const expectedPhysical = rows.map((row) => bytes(row.physical));
                const f = fixture({ defs: definitions(1), rows });
                try {
                    const tx = await f.worker.begin(mode);
                    const savedResolve = f.worker.resolveVisibleIndexedValue;
                    let primaryKeySlices = 0;
                    let got;
                    f.worker.resolveVisibleIndexedValue = function (...args) {
                        const primaryKey = args[3].primaryKey;
                        const savedSlice = primaryKey.slice;
                        // Observe only this decoded key. Native prototypes and
                        // the decoder's independent ownership stay unchanged.
                        Object.defineProperty(primaryKey, 'slice', {
                            value(...sliceArgs) {
                                primaryKeySlices++;
                                return savedSlice.apply(this, sliceArgs);
                            }
                        });
                        return savedResolve.apply(this, args);
                    };
                    try {
                        got = await f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 2 });
                    } finally {
                        f.worker.resolveVisibleIndexedValue = savedResolve;
                    }
                    eq(
                        got.map((row) => bytes(row.key)),
                        expectedKeys
                    );
                    eq(
                        got.map((row) => bytes(row.value)),
                        rows.map((row) => bytes(row.value))
                    );
                    for (const [index, row] of got.entries()) {
                        const source = rows.at(index);
                        ok(row.key.buffer !== source.key.buffer, 'key aliases caller primary bytes');
                        ok(row.key.buffer !== source.physical.buffer, 'key aliases caller physical bytes');
                        eq(row.key.byteOffset, 0);
                        eq(row.key.buffer.byteLength, 257);
                    }
                    ok(got[0].key.buffer !== got[1].key.buffer, 'returned keys share their ownership buffer');
                    got[0].key.fill(0xee);
                    eq(
                        rows.map((row) => bytes(row.key)),
                        expectedKeys,
                        'returned key mutation changed caller keys'
                    );
                    eq(
                        rows.map((row) => bytes(row.physical)),
                        expectedPhysical,
                        'returned key mutation changed physical index keys'
                    );
                    const again = await f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 2 });
                    eq(
                        again.map((row) => bytes(row.key)),
                        expectedKeys,
                        'returned key mutation changed the next scan'
                    );
                    eq(
                        again.map((row) => bytes(row.value)),
                        rows.map((row) => bytes(row.value))
                    );
                    const values = await Promise.all(rows.map((row) => f.worker.get(tx, 'docs', row.key)));
                    eq(
                        values.map(bytes),
                        rows.map((row) => bytes(row.value)),
                        'returned key mutation changed primary lookup or document'
                    );
                    await f.worker.rollback(tx);
                    eq(primaryKeySlices, 0, 'decoded primary keys were copied again');
                } finally {
                    f.close();
                }
            },
            true
        );
    }
    for (const reverse of [false, true]) {
        for (const limit of [1, 100, 256]) {
            test(
                `clean index scan reads only demand: reverse=${reverse}, limit=${limit}`,
                async () => {
                    const f = fixture({ defs: definitions(1), rows: indexRows(2000) });
                    const tx = await f.worker.begin('readonly');
                    const got = await f.worker.scanByIndex(tx, 'docs', 'i0', { reverse, limit });
                    eq(got.length, limit);
                    eq(f.counts.rawRows, limit);
                    eq(f.counts.gets, limit);
                    eq(f.counts.getCalls, limit === 1 ? 1 : 0);
                    eq(f.counts.getManyCalls, limit === 1 ? 0 : 1);
                    const expected = Array.from({ length: limit }, (_, i) => hex(u64Key(reverse ? 1999 - i : i)));
                    eq(
                        got.map((r) => hex(r.key)),
                        expected
                    );
                    f.close();
                },
                true
            );
        }
        for (const mode of ['readonly', 'readwrite']) {
            test(`bounded continuation with missing/stale rows, reverse=${reverse}, mode=${mode}`, async () => {
                const f = fixture({
                    defs: definitions(1),
                    rows: indexRows(
                        1500,
                        (i) => i % 3 !== 0,
                        (i) => i % 5 === 0
                    )
                });
                const tx = await f.worker.begin(mode);
                const range = { gte: indexKey(100), lt: indexKey(1400), reverse };
                let cursor = null;
                const got = [];
                for (let n = 0; n < 100; n++) {
                    const page = await f.worker.scanByIndexPage(tx, 'docs', 'i0', range, cursor, 37);
                    got.push(...page.rows.map((r) => hex(r.key)));
                    if (page.cursor === null) break;
                    cursor = page.cursor;
                    ok(n !== 99, 'pagination did not terminate');
                }
                const expected = Array.from({ length: 1300 }, (_, i) => i + 100).filter(
                    (i) => i % 3 !== 0 && i % 5 !== 0
                );
                if (reverse) expected.reverse();
                eq(
                    got,
                    expected.map((i) => hex(u64Key(i)))
                );
                eq(new Set(got).size, got.length);
                if (mode === 'readwrite') {
                    ok(f.counts.deletes > 0, 'stale cleanup did not execute');
                    eq(f.counts.getManyCalls, 0);
                } else {
                    eq(f.counts.deletes, 0);
                    ok(f.counts.getManyCalls > 0, 'readonly rows did not use bulk lookups');
                }
                f.close();
            });
        }
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
    test(
        'getByIndex reads one physical entry for a large duplicate-key range',
        async () => {
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
        },
        true
    );
    test('index error propagation and zero limits', async () => {
        const f = fixture({ defs: definitions(1) });
        const tx = await f.worker.begin('readonly');
        eq(await f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 0 }), []);
        await rejects(() => f.worker.scanByIndexPage(tx, 'docs', 'i0', {}, null, 0), 'InvalidRangeError');
        f.worker.engine.scan = () => {
            throw named('CorruptionError');
        };
        await rejects(() => f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 1 }), 'CorruptionError');
        f.close();
    });
    test('readonly index batches stop before an unreachable corrupt value in a stale chunk', async () => {
        const rows = indexRows(20, (i) => i >= 6);
        rows[8].value = new Uint8Array([255]);
        const f = fixture({ defs: definitions(1), rows });
        const tx = await f.worker.begin('readonly');
        const got = await f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 2 });
        eq(
            got.map((row) => hex(row.key)),
            [hex(u64Key(6)), hex(u64Key(7))]
        );
        eq(f.counts.gets, 8);
        eq(f.counts.deletes, 0);
        f.close();
    });
    test('failed readonly bulk lookup preserves the earlier document error', async () => {
        const rows = indexRows(10);
        rows[0].value = new Uint8Array([255]);
        const f = fixture({ defs: definitions(1), rows });
        const tx = await f.worker.begin('readonly');
        f.worker.engine.get_many = () => {
            f.counts.getManyCalls++;
            throw named('CorruptionError');
        };
        await rejects(() => f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 10 }), 'SerializationError');
        eq(f.counts.getManyCalls, 1);
        eq(f.counts.getCalls, 1);
        eq(f.counts.deletes, 0);
        f.close();
    });
    test('a malformed later index key does not precede an earlier document error', async () => {
        const rows = indexRows(10);
        rows[0].value = new Uint8Array([255]);
        const f = fixture({ defs: definitions(1), rows });
        const tx = await f.worker.begin('readonly');
        const scan = f.worker.engine.scan;
        f.worker.engine.scan = (id, store, range) => {
            const selected = scan(id, store, range);
            if (store !== 'docs' && selected.length > 1) selected[1].key = new Uint8Array([255]);
            return selected;
        };
        await rejects(() => f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 10 }), 'SerializationError');
        eq(f.counts.getCalls, 1);
        eq(f.counts.getManyCalls, 0);
        f.close();
    });
    test('readonly bulk failure falls back in row order and preserves its engine error', async () => {
        const f = fixture({ defs: definitions(1), rows: indexRows(10) });
        const tx = await f.worker.begin('readonly');
        const get = f.worker.engine.get;
        f.worker.engine.get_many = () => {
            f.counts.getManyCalls++;
            throw named('CorruptionError');
        };
        f.worker.engine.get = (id, store, key) => {
            if (hex(key) === hex(u64Key(4))) throw named('CorruptionError');
            return get(id, store, key);
        };
        await rejects(() => f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 10 }), 'CorruptionError');
        eq(f.counts.getCalls, 4);
        eq(f.counts.getManyCalls, 1);
        f.close();
    });
    test('write transaction scans preserve stale cleanup before a document error', async () => {
        const rows = indexRows(10, (i) => i !== 0);
        rows[1].value = new Uint8Array([255]);
        const f = fixture({ defs: definitions(1), rows });
        const tx = await f.worker.begin('readwrite');
        await rejects(() => f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 10 }), 'SerializationError');
        eq(f.counts.getManyCalls, 0);
        eq(f.counts.getCalls, 2);
        eq(f.counts.deletes, 1);
        eq(f.physical.has(hex(rows[0].physical)), false);
        f.close();
    });
    test('readonly bulk outcome size mismatch is an explicit internal error', async () => {
        const f = fixture({ defs: definitions(1), rows: indexRows(10) });
        const tx = await f.worker.begin('readonly');
        f.worker.engine.get_many = () => [];
        await rejects(() => f.worker.scanByIndex(tx, 'docs', 'i0', { limit: 10 }), 'InternalError', 'outcome count');
        f.close();
    });
    test(
        'lazy extractor preserves scalar, compound, missing and null behavior',
        async () => {
            const defs = indexing.normalizeIndexDefinitions([
                { store: 'docs', name: 'a', keyPath: 'name' },
                { store: 'docs', name: 'b', keyPath: ['name', 'nested.enabled'] },
                { store: 'docs', name: 'c', keyPath: 'absent' },
                { store: 'docs', name: 'd', keyPath: 'nested.enabled' }
            ]);
            for (const doc of [
                null,
                false,
                42,
                'text',
                [],
                { name: null, nested: { enabled: false } },
                { name: 'A\u0000Б', nested: { enabled: true } },
                { name: 'partial' }
            ]) {
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
        },
        true
    );
    test(
        'insert decodes once; absent delete and unindexed bytes require no JSON',
        async () => {
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
            const c = await counted(() =>
                raw.worker.autocommit('readwrite', 'put', ['docs', u64Key(1), new Uint8Array([255])])
            );
            eq(c.jsonParses, 0);
            eq(raw.counts.commit, 1);
            eq(raw.counts.rollback, 0);
            raw.close();
        },
        true
    );
    test('autocommit validation failure rolls back and releases metadata', async () => {
        const f = fixture({ defs: definitions(8), document: document(8) });
        await rejects(
            () => f.worker.autocommit('readwrite', 'put', ['docs', u64Key(1), new Uint8Array([255])]),
            'SerializationError'
        );
        eq(f.counts.rollback, 1);
        eq(f.counts.commit, 0);
        eq(f.writes.length, 0);
        eq(f.worker.txStoreCompression.size, 0);
        f.close();
    });
    async function runTests(structural = true) {
        const results = [];
        for (const t of tests) {
            if (!structural && t.structural) continue;
            try {
                await t.fn();
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
    async function measureWork() {
        const metadata = [];
        for (const stores of [1, 100, 10000]) {
            for (const mode of ['readonly', 'readwrite']) {
                const f = fixture({ stores });
                const c = await counted(async () => {
                    const tx = await f.worker.begin(mode);
                    if (mode === 'readonly') await f.worker.rollback(tx);
                    else await f.worker.commit(tx);
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
            indexed.push({
                indexes,
                putParses: put.jsonParses,
                putDecodes: put.utf8Decodes,
                deleteParses: del.jsonParses
            });
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
        const d = document(8, '', 65536),
            key = u64Key(1);
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
                    for (let i = 0; i < 10; i++) await stale.worker.scanByIndex(tx, 'docs', 'i0', { limit: 1 });
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
                    for (let i = 0; i < 100; i++) await indexed.worker.put(tx, 'docs', key, d);
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
            close() {
                metadata.close();
                indexed.close();
                scan.close();
                small.close();
                stale.close();
            }
        };
    }
    return { runTests, measureWork, makeBenchmarks };
}
