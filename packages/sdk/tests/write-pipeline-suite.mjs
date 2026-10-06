import assert from 'node:assert/strict';

export async function checkWritePipeline({ DbWorker, indexing, codec, compression, protocol }) {
    const results = [];
    const test = async (name, run) => {
        try {
            const evidence = await run();
            results.push({ name, passed: true, ...(evidence ? { evidence } : {}) });
        } catch (error) {
            results.push({ name, passed: false, error: String(error.stack ?? error) });
        }
    };
    const named = (name) => Object.assign(new Error(name), { name });
    const hex = (bytes) => Buffer.from(bytes).toString('hex');
    const compare = (a, b) => Buffer.compare(a, b);
    const document = (email, padding = '') => codec.jsonEncode({ email, padding });
    function packPairs(entries) {
        const items = entries.flat();
        const output = new Uint8Array(4 + items.length * 4 + items.reduce((sum, value) => sum + value.byteLength, 0));
        const view = new DataView(output.buffer);
        view.setUint32(0, items.length, true);
        let offset = 4 + items.length * 4;
        for (let index = 0; index < items.length; index++) {
            view.setUint32(4 + index * 4, items[index].byteLength, true);
            output.set(items[index], offset);
            offset += items[index].byteLength;
        }
        return output;
    }
    function fixture({
        kind = false,
        unique,
        docs = [],
        nativeSizes = false,
        nativeRevisions = false,
        nativeKeyScan = false
    } = {}) {
        const worker = new DbWorker({
            persistence: { close() {}, persisted: async () => false, persist: async () => false }
        });
        const defs =
            unique === undefined
                ? []
                : indexing.normalizeIndexDefinitions([{ store: 'docs', name: 'email', keyPath: 'email', unique }]);
        const stores = new Map([['docs', new Map()]]);
        for (const def of defs) stores.set(def.internalStore, new Map());
        const counts = {
            packedCalls: 0,
            tupleCalls: 0,
            scalarCalls: 0,
            indexedCalls: 0,
            primaryReads: 0,
            getManyCalls: 0,
            metadataCalls: 0,
            stateCalls: 0,
            primaryBodyReads: 0,
            checkedStamps: 0,
            expiryStages: 0,
            indexBodyBytes: 0,
            keyPages: 0,
            keyCursorCloses: 0,
            batchLogicalBytes: [],
            packets: []
        };
        let indexFailure;
        let readFailure;
        let now = 1000;
        let ordinal = 1n;
        const snapshots = new Map();
        const writerSnapshots = new Map();
        const copyStores = () =>
            new Map(
                Array.from(stores, ([name, rows]) => [
                    name,
                    new Map(
                        Array.from(rows, ([key, row]) => [
                            key,
                            { ...row, key: row.key.slice(), value: row.value.slice() }
                        ])
                    )
                ])
            );
        worker.committedIndexes = defs;
        worker.committedStoreCompression = new Map([['docs', kind]]);
        worker.txModes.set(1, 'readwrite');
        const storeView = (txId) => snapshots.get(Number(txId)) ?? stores;
        const read = (store, key, txId) => storeView(txId).get(store)?.get(hex(key));
        const write = (store, key, value, revision = null, expiresAt = null) => {
            const rows = stores.get(store);
            if (!rows) throw named('StoreNotFoundError');
            const existed = rows.has(hex(key));
            rows.set(hex(key), {
                key: key.slice(),
                value: value.slice(),
                revision: revision ?? (nativeRevisions ? { epoch: 1n, ordinal: ordinal++ } : null),
                expiresAt
            });
            return existed;
        };
        for (const [key, value] of docs) {
            write('docs', key, value);
            for (const def of defs) {
                const logical = indexing.extractLogicalIndexKey(def, value);
                if (logical !== null)
                    write(def.internalStore, indexing.encodeIndexEntryKey(logical, key), new Uint8Array());
            }
        }
        const unpack = (packet) => {
            const flat = protocol.unpackPackedBinaryList(packet);
            assert.equal(flat.length % 2, 0);
            return Array.from({ length: flat.length / 2 }, (_, index) => [flat[index * 2], flat[index * 2 + 1]]);
        };
        worker.engine = {
            needs_recovery: () => false,
            create_store(_tx, store) {
                if (stores.has(store)) throw named('StoreExistsError');
                stores.set(store, new Map());
            },
            get(_tx, store, key) {
                if (store === 'docs') counts.primaryReads++;
                if (readFailure?.(store, key)) throw named('CorruptionError');
                const row = read(store, key, _tx);
                if (!row) return null;
                if (row.expiresAt !== null && row.expiresAt <= now) {
                    if (worker.txModes.get(Number(_tx)) === 'readwrite') {
                        stores.get(store).delete(hex(key));
                        counts.expiryStages++;
                    }
                    return null;
                }
                if (store === 'docs') counts.primaryBodyReads++;
                return row.value.slice();
            },
            get_many(_tx, store, keys) {
                counts.getManyCalls++;
                counts.primaryReads += keys.length;
                const values = keys.map((key) => {
                    const row = read(store, key, _tx);
                    if (!row || (row.expiresAt !== null && row.expiresAt <= now)) return null;
                    if (store === 'docs') counts.primaryBodyReads++;
                    return row.value.slice();
                });
                counts.lastBatchValues = values;
                counts.batchLogicalBytes.push(
                    values.reduce((sum, value, index) => {
                        const logicalBytes =
                            kind && value?.byteLength >= 18
                                ? new DataView(value.buffer, value.byteOffset, value.byteLength).getUint32(10, true)
                                : (value?.byteLength ?? 0);
                        return (
                            sum +
                            (value === null ? 0 : 8 + keys[index].byteLength + Math.max(value.byteLength, logicalBytes))
                        );
                    }, 0)
                );
                return values;
            },
            scan(_tx, store, range) {
                const current = storeView(_tx);
                if (!current.has(store)) throw named('StoreNotFoundError');
                let rows = Array.from(current.get(store).values())
                    .filter((row) => row.expiresAt === null || row.expiresAt === undefined || row.expiresAt > now)
                    .sort((a, b) => compare(a.key, b.key));
                if (range.reverse) rows.reverse();
                rows = rows.filter(
                    (row) =>
                        (range.gt === undefined || compare(row.key, range.gt) > 0) &&
                        (range.gte === undefined || compare(row.key, range.gte) >= 0) &&
                        (range.lt === undefined || compare(row.key, range.lt) < 0) &&
                        (range.lte === undefined || compare(row.key, range.lte) <= 0)
                );
                if (store.startsWith('__browserdb:index:'))
                    counts.indexBodyBytes += rows
                        .slice(0, range.limit ?? Infinity)
                        .reduce((sum, row) => sum + row.value.byteLength, 0);
                return rows
                    .slice(0, range.limit ?? Infinity)
                    .map((row) => ({ key: row.key.slice(), value: row.value.slice() }));
            },
            delete(_tx, store, key) {
                return stores.get(store)?.delete(hex(key)) ?? false;
            },
            has(_tx, store, key) {
                const row = read(store, key, _tx);
                if (!row) return false;
                if (row.expiresAt !== null && row.expiresAt <= now) {
                    if (worker.txModes.get(Number(_tx)) === 'readwrite') {
                        stores.get(store).delete(hex(key));
                        counts.expiryStages++;
                    }
                    return false;
                }
                return true;
            },
            clear_store(_tx, store) {
                stores.get(store)?.clear();
            },
            drop_store(_tx, store) {
                stores.delete(store);
            },
            rollback_tx(txId) {
                const saved = writerSnapshots.get(Number(txId));
                if (saved) {
                    stores.clear();
                    for (const [name, rows] of saved) stores.set(name, rows);
                }
            },
            put(_tx, store, key, value) {
                counts.scalarCalls++;
                if (nativeRevisions) return write(store, key, value);
                throw new Error('unexpected scalar put');
            },
            put_many() {
                counts.tupleCalls++;
                throw new Error('unexpected tuple put_many');
            },
            put_many_packed(_tx, store, packet) {
                counts.packedCalls++;
                counts.packets.push(packet);
                return Uint8Array.from(unpack(packet), ([key, value]) => Number(write(store, key, value)));
            },
            put_many_indexed_packed(_tx, store, packet, operations, options = {}) {
                counts.indexedCalls++;
                counts.packets.push(packet);
                const partial = [];
                try {
                    for (const [[key, value], index] of unpack(packet).map((entry, index) => [entry, index])) {
                        const existed = write(
                            store,
                            key,
                            value,
                            null,
                            options.ttl === undefined ? null : now + Number(options.ttl)
                        );
                        for (const op of operations[index]) {
                            if (indexFailure?.(op, key)) throw named('CorruptionError');
                            if (op.kind === 'delete') stores.get(op.store).delete(hex(op.key));
                            else
                                write(
                                    op.store,
                                    op.key,
                                    new Uint8Array(),
                                    nativeRevisions ? read(store, key).revision : null
                                );
                        }
                        partial.push(existed);
                    }
                } catch (error) {
                    error.partial = partial;
                    throw error;
                }
                return Uint8Array.from(partial, Number);
            }
        };
        if (nativeKeyScan) {
            let nextCursor = 1;
            const cursors = new Map();
            worker.engine.open_scan_cursor = (txId, store, range, keysOnly) => {
                assert.equal(keysOnly, true);
                if (!storeView(txId).has(store)) throw named('StoreNotFoundError');
                const id = nextCursor++;
                cursors.set(id, { txId, store, range });
                return id;
            };
            worker.engine.scan_cursor_next = (id, maxRows, maxBytes) => {
                const { txId, store, range } = cursors.get(id);
                let rows = Array.from(storeView(txId).get(store).values())
                    .filter((row) => row.expiresAt === null || row.expiresAt === undefined || row.expiresAt > now)
                    .sort((a, b) => compare(a.key, b.key));
                if (range.reverse) rows.reverse();
                rows = rows
                    .filter(
                        (row) =>
                            (range.gt === undefined || compare(row.key, range.gt) > 0) &&
                            (range.gte === undefined || compare(row.key, range.gte) >= 0) &&
                            (range.lt === undefined || compare(row.key, range.lt) < 0) &&
                            (range.lte === undefined || compare(row.key, range.lte) <= 0)
                    )
                    .slice(0, Math.min(maxRows, range.limit ?? maxRows));
                const packet = new Uint8Array(4 + rows.reduce((sum, row) => sum + 8 + row.key.byteLength, 0));
                assert.ok(packet.byteLength <= maxBytes);
                const view = new DataView(packet.buffer);
                view.setUint32(0, rows.length, true);
                let offset = 4;
                for (const row of rows) {
                    view.setUint32(offset, row.key.byteLength, true);
                    offset += 8;
                    packet.set(row.key, offset);
                    offset += row.key.byteLength;
                }
                counts.keyPages++;
                return { packet, rowCount: rows.length, exhausted: rows.length < maxRows };
            };
            worker.engine.close_scan_cursor = (id) => {
                assert.ok(cursors.delete(id));
                counts.keyCursorCloses++;
            };
        }
        if (nativeSizes) {
            worker.engine.get_many_value_sizes = (_tx, store, packedKeys) => {
                counts.metadataCalls++;
                return Uint32Array.from(protocol.unpackPackedBinaryList(packedKeys), (key) => {
                    const value = read(store, key)?.value;
                    if (value === undefined) return 0xffff_ffff;
                    const logicalBytes =
                        kind && value.byteLength >= 18
                            ? new DataView(value.buffer, value.byteOffset, value.byteLength).getUint32(10, true)
                            : value.byteLength;
                    return Math.max(value.byteLength, logicalBytes);
                });
            };
        }
        if (nativeRevisions) {
            worker.engine.get_many_value_states = (_tx, store, packedKeys) => {
                counts.stateCalls++;
                const keys = protocol.unpackPackedBinaryList(packedKeys);
                const packet = new Uint8Array(4 + keys.length * 32);
                const view = new DataView(packet.buffer);
                view.setUint32(0, keys.length, true);
                for (let index = 0; index < keys.length; index++) {
                    const row = read(store, keys[index], _tx);
                    if (!row) continue;
                    const offset = 4 + index * 32;
                    const expired = row.expiresAt !== null && row.expiresAt <= now;
                    view.setUint32(offset, (expired ? 2 : 1) | (row.revision ? 4 : 0), true);
                    view.setBigUint64(offset + 4, BigInt(row.expiresAt ?? 0), true);
                    view.setBigUint64(offset + 12, row.revision?.epoch ?? 0n, true);
                    view.setBigUint64(offset + 20, row.revision?.ordinal ?? 0n, true);
                    const logical =
                        kind && store === 'docs' && row.value.byteLength >= 18
                            ? new DataView(row.value.buffer, row.value.byteOffset, row.value.byteLength).getUint32(
                                  10,
                                  true
                              )
                            : row.value.byteLength;
                    view.setUint32(offset + 28, expired ? 0 : Math.max(row.value.byteLength, logical), true);
                }
                return packet;
            };
            worker.engine.put_index_entry_checked = (_tx, primaryStore, indexStore, key, physical, epoch, sequence) => {
                const row = read(primaryStore, key, _tx);
                if (!row?.revision || row.revision.epoch !== epoch || row.revision.ordinal !== sequence)
                    throw named('TransactionConflictError');
                assert.deepEqual(indexing.decodeIndexEntryKey(physical).primaryKey, key);
                write(indexStore, physical, new Uint8Array(), row.revision);
                counts.checkedStamps++;
            };
        }
        return {
            worker,
            stores,
            defs,
            counts,
            rawWrite(store, key, value, expiresAt = null) {
                write(store, key, value, null, expiresAt);
            },
            snapshot(txId) {
                snapshots.set(txId, copyStores());
                worker.txModes.set(txId, 'readonly');
            },
            checkpointWriter(txId = 1) {
                writerSnapshots.set(txId, copyStores());
            },
            setNow(value) {
                now = value;
            },
            failIndex(fn) {
                indexFailure = fn;
            },
            failRead(fn) {
                readFailure = fn;
            },
            events() {
                return worker.finalizeTrackedChanges(worker.txChanges.get(1) ?? new Map());
            },
            close() {
                worker.persistenceBridge.close();
            }
        };
    }
    function cursorFixture({ kind = false, rows = [] } = {}) {
        const worker = new DbWorker({
            persistence: { close() {}, persisted: async () => false, persist: async () => false }
        });
        worker.committedIndexes = [];
        worker.committedStoreCompression = new Map([['docs', kind]]);
        const transactions = new Map();
        const cursors = new Map();
        const counts = { begins: 0, rollbacks: 0, closes: 0, pages: 0 };
        let committed = rows;
        let nextTx = 1;
        let nextCursor = 1;
        let lastPacket;
        const packet = (page) => {
            const result = new Uint8Array(
                4 + page.reduce((sum, row) => sum + 8 + row.key.byteLength + row.value.byteLength, 0)
            );
            const view = new DataView(result.buffer);
            view.setUint32(0, page.length, true);
            let offset = 4;
            for (const row of page) {
                view.setUint32(offset, row.key.byteLength, true);
                view.setUint32(offset + 4, row.value.byteLength, true);
                offset += 8;
                result.set(row.key, offset);
                offset += row.key.byteLength;
                result.set(row.value, offset);
                offset += row.value.byteLength;
            }
            return result;
        };
        worker.engine = {
            needs_recovery: () => false,
            begin_tx() {
                const id = nextTx++;
                transactions.set(
                    id,
                    committed.map((row) => ({ key: row.key.slice(), value: row.value.slice() }))
                );
                counts.begins++;
                return BigInt(id);
            },
            rollback_tx(id) {
                transactions.delete(Number(id));
                counts.rollbacks++;
            },
            open_scan_cursor(id, store, range) {
                if (store !== 'docs') throw named('StoreNotFoundError');
                if (!transactions.has(Number(id))) throw named('TransactionClosedError');
                const cursor = nextCursor++;
                let selected = transactions.get(Number(id));
                selected = selected.filter(
                    (row) =>
                        (range.gt === undefined || compare(row.key, range.gt) > 0) &&
                        (range.gte === undefined || compare(row.key, range.gte) >= 0) &&
                        (range.lt === undefined || compare(row.key, range.lt) < 0) &&
                        (range.lte === undefined || compare(row.key, range.lte) <= 0)
                );
                if (range.reverse) selected = selected.toReversed();
                cursors.set(cursor, { rows: selected.slice(0, range.limit ?? Infinity), offset: 0 });
                return cursor;
            },
            scan_cursor_next(id, maxRows, maxBytes) {
                const cursor = cursors.get(id);
                if (!cursor) throw named('TransactionClosedError');
                const page = [];
                let budget = 4;
                while (cursor.offset < cursor.rows.length && page.length < maxRows) {
                    const row = cursor.rows[cursor.offset];
                    const logicalBytes =
                        kind && row.value.byteLength >= 18
                            ? new DataView(row.value.buffer, row.value.byteOffset, row.value.byteLength).getUint32(
                                  10,
                                  true
                              )
                            : row.value.byteLength;
                    const required = 8 + row.key.byteLength + Math.max(row.value.byteLength, logicalBytes);
                    if (budget + required > maxBytes) {
                        if (page.length === 0) throw named('ValueTooLargeError');
                        break;
                    }
                    budget += required;
                    page.push(row);
                    cursor.offset++;
                }
                lastPacket = packet(page);
                counts.pages++;
                return { packet: lastPacket, rowCount: page.length, exhausted: cursor.offset === cursor.rows.length };
            },
            close_scan_cursor(id) {
                cursors.delete(id);
                counts.closes++;
            }
        };
        return {
            worker,
            counts,
            cursors,
            transactions,
            packet: () => lastPacket,
            update(next) {
                committed = next;
            }
        };
    }

    await test('Snappy packed writes use one native packet and no scalar or tuple API', async () => {
        const f = fixture({ kind: 'snappy' });
        const entries = Array.from({ length: 100 }, (_, index) => [
            codec.u64Key(index),
            new Uint8Array(4096).fill(index)
        ]);
        await f.worker.putManyPacked(1, 'docs', packPairs(entries));
        assert.equal(f.counts.packedCalls, 1);
        assert.equal(f.counts.tupleCalls + f.counts.scalarCalls, 0);
        assert.ok(f.counts.packets[0].byteLength < (100 * 4096) / 4);
        for (const [key, raw] of entries)
            assert.deepEqual(
                await compression.decodeStoreValueRecord(f.stores.get('docs').get(hex(key)).value, {
                    strict: true
                }),
                raw
            );
        return {
            nativePackedCalls: 1,
            scalarCalls: 0,
            tupleCalls: 0,
            inputBytes: packPairs(entries).byteLength,
            packedBytes: f.counts.packets[0].byteLength
        };
    });
    await test('small Snappy records allocate one final packet and no value envelopes', async () => {
        const f = fixture({ kind: 'snappy' });
        const entries = Array.from({ length: 24 }, (_, index) => [codec.u64Key(index), Uint8Array.of(index)]);
        const packet = packPairs(entries);
        const Original = globalThis.Uint8Array;
        const numericAllocations = [];
        globalThis.Uint8Array = new Proxy(Original, {
            construct(target, args) {
                if (typeof args[0] === 'number' && args[0] > 0) numericAllocations.push(args[0]);
                return Reflect.construct(target, args, target);
            }
        });
        try {
            await f.worker.putManyPacked(1, 'docs', packet);
        } finally {
            globalThis.Uint8Array = Original;
        }
        assert.deepEqual(numericAllocations, [4 + 24 * (8 + 8 + 19), 24]);
        return { finalPacketAllocations: 1, nativeOutcomeAllocations: 1, perRecordEnvelopeAllocations: 0 };
    });
    await test('compression false forwards the original packet', async () => {
        const f = fixture();
        const packet = packPairs([[codec.u64Key(0), Uint8Array.of(1)]]);
        await f.worker.putManyPacked(1, 'docs', packet);
        assert.equal(f.counts.packets[0], packet);
    });
    await test('new public stores default to Snappy with an explicit false opt-out', async () => {
        const f = fixture();
        const options = [];
        f.worker.engine.create_store = (_id, name, option) => options.push({ name, ...option });
        await f.worker.createStore(1, 'default');
        await f.worker.createStore(1, 'raw', { compression: false });
        await f.worker.createStore(1, indexing.INDEX_METADATA_STORE, { compression: 'gzip' });
        assert.deepEqual(options, [
            { name: 'default', compression: 'snappy' },
            { name: 'raw', compression: false },
            { name: indexing.INDEX_METADATA_STORE, compression: false }
        ]);
    });
    await test('malformed packed input fails before writes', async () => {
        for (const packet of [Uint8Array.of(1), Uint8Array.of(1, 0, 0, 0), Uint8Array.of(0, 0, 0, 0, 1)]) {
            const f = fixture({ kind: 'snappy' });
            await assert.rejects(f.worker.putManyPacked(1, 'docs', packet), { name: 'WorkerProtocolError' });
            assert.equal(f.counts.packedCalls, 0);
        }
    });
    await test('independent indexed writes share ordered native batches', async () => {
        const f = fixture({ unique: false });
        const entries = Array.from({ length: 150 }, (_, index) => [codec.u64Key(index), document(`e${index}`)]);
        await f.worker.putManyPacked(1, 'docs', packPairs(entries));
        assert.equal(f.counts.indexedCalls, 3);
        assert.equal(f.counts.scalarCalls + f.counts.tupleCalls, 0);
        assert.equal(f.counts.primaryReads, 150);
        assert.equal(f.stores.get(f.defs[0].internalStore).size, 150);
        return { indexedNativeCalls: 3, documents: 150, primaryValidationReads: 150 };
    });
    await test('duplicate keys keep source order and remove intermediate index rows', async () => {
        const key = codec.u64Key(1);
        const f = fixture({ unique: false });
        await f.worker.putMany(1, 'docs', [
            [key, document('first')],
            [key, document('second')],
            [codec.u64Key(2), document('third')]
        ]);
        assert.equal(f.counts.indexedCalls, 2);
        const rows = [...f.stores.get(f.defs[0].internalStore).values()].map(
            (row) => indexing.decodeIndexEntryKey(row.key).logicalKey
        );
        assert.equal(rows.length, 2);
        assert.ok(!rows.some((value) => compare(value, codec.indexKey('first')) === 0));
        assert.deepEqual(codec.jsonDecode(f.stores.get('docs').get(hex(key)).value), { email: 'second', padding: '' });
    });
    await test('unique conflicts flush successful prefix and keep operation events', async () => {
        const f = fixture({ unique: true });
        await assert.rejects(
            f.worker.putMany(1, 'docs', [
                [codec.u64Key(1), document('same')],
                [codec.u64Key(2), document('same')],
                [codec.u64Key(3), document('last')]
            ]),
            { name: 'UniqueIndexConstraintError' }
        );
        assert.equal(f.stores.get('docs').size, 1);
        assert.equal(f.events()[0].changes.length, 1);
        assert.equal(f.counts.indexedCalls, 1);
    });
    await test('a freed unique key is visible to the next batch row', async () => {
        const first = codec.u64Key(1);
        const f = fixture({ unique: true, docs: [[first, document('old')]] });
        await f.worker.putMany(1, 'docs', [
            [first, document('new')],
            [codec.u64Key(2), document('old')]
        ]);
        assert.equal(f.stores.get('docs').size, 2);
        assert.equal(f.stores.get(f.defs[0].internalStore).size, 2);
        assert.equal(f.counts.indexedCalls, 2);
    });
    await test('index write errors do not write later primaries or replay the batch', async () => {
        const f = fixture({ unique: false });
        f.failIndex((_op, key) => hex(key) === hex(codec.u64Key(2)));
        await assert.rejects(
            f.worker.putMany(
                1,
                'docs',
                [1, 2, 3].map((index) => [codec.u64Key(index), document(`e${index}`)])
            ),
            { name: 'CorruptionError' }
        );
        assert.equal(f.stores.get('docs').size, 2);
        assert.equal(f.stores.get(f.defs[0].internalStore).size, 1);
        assert.equal(f.events()[0].changes.length, 1);
        assert.equal(f.counts.indexedCalls, 1);
    });
    await test('document corruption preserves completed earlier rows', async () => {
        const f = fixture({ unique: false });
        await assert.rejects(
            f.worker.putMany(1, 'docs', [
                [codec.u64Key(1), document('first')],
                [codec.u64Key(2), Uint8Array.of(255)],
                [codec.u64Key(3), document('third')]
            ]),
            { name: 'SerializationError' }
        );
        assert.equal(f.stores.get('docs').size, 1);
        assert.equal(f.events()[0].changes.length, 1);
    });
    await test('unique stale entries still validate the actual primary document', async () => {
        const key = codec.u64Key(9);
        const f = fixture({ unique: true, docs: [[key, document('current')]] });
        const def = f.defs[0];
        const stale = indexing.encodeIndexEntryKey(codec.indexKey('stale'), key);
        f.stores.get(def.internalStore).set(hex(stale), { key: stale, value: new Uint8Array() });
        await f.worker.putMany(1, 'docs', [[codec.u64Key(1), document('stale')]]);
        assert.equal(f.counts.primaryReads, 2);
        assert.equal(f.stores.get(def.internalStore).size, 2);
    });
    await test('TTL writes use row boundaries and revalidate duplicate primary rows', async () => {
        const f = fixture({ unique: false });
        await f.worker.putMany(
            1,
            'docs',
            [1, 2, 3].map((index) => [codec.u64Key(index), document(`e${index}`)]),
            { ttl: 1 }
        );
        assert.equal(f.counts.indexedCalls, 3);
        assert.equal(f.counts.primaryReads, 3);
    });
    await test('raw cursor pages reuse the engine packet and keep their snapshot', async () => {
        const rows = [1, 2, 3].map((value) => ({ key: Uint8Array.of(value), value: Uint8Array.of(value + 10) }));
        const f = cursorFixture({ rows });
        const first = await f.worker.scanPage({ store: 'docs', maxRows: 1, maxBytes: 64 });
        assert.equal(first.rows.bytes, f.packet());
        assert.equal(first.done, false);
        assert.equal(first.bytes, 14);
        f.update([{ key: Uint8Array.of(9), value: Uint8Array.of(99) }]);
        const second = await f.worker.scanPage({ store: 'docs', cursorId: first.cursorId, maxRows: 8, maxBytes: 64 });
        assert.deepEqual(
            protocol.unpackPackedScanRows(second.rows.bytes).map((row) => row.key[0]),
            [2, 3]
        );
        assert.equal(second.done, true);
        assert.equal(second.cursorId, undefined);
        assert.deepEqual(f.counts, { begins: 1, rollbacks: 1, closes: 1, pages: 2 });
        assert.equal(f.worker.primaryCursors.size + f.transactions.size + f.cursors.size, 0);
    });
    await test('cursor close is idempotent and does not close a caller transaction', async () => {
        const f = cursorFixture({
            rows: [1, 2].map((value) => ({ key: Uint8Array.of(value), value: Uint8Array.of(value) }))
        });
        const txId = await f.worker.begin('readonly');
        const page = await f.worker.scanPage({ txId, store: 'docs', maxRows: 1, maxBytes: 64 });
        await f.worker.closeCursor(page.cursorId);
        await f.worker.closeCursor(page.cursorId);
        assert.equal(f.counts.rollbacks, 0);
        assert.equal(f.transactions.size, 1);
        await f.worker.rollback(txId);
        assert.equal(f.counts.rollbacks, 1);
    });
    await test('transaction cleanup closes its live cursors', async () => {
        const f = cursorFixture({
            rows: [1, 2].map((value) => ({ key: Uint8Array.of(value), value: Uint8Array.of(value) }))
        });
        const txId = await f.worker.begin('readonly');
        const page = await f.worker.scanPage({ txId, store: 'docs', maxRows: 1, maxBytes: 64 });
        await f.worker.rollback(txId);
        assert.equal(f.cursors.size + f.worker.primaryCursors.size, 0);
        await assert.rejects(
            f.worker.scanPage({ txId, cursorId: page.cursorId, store: 'docs', maxRows: 1, maxBytes: 64 }),
            { name: 'TransactionClosedError' }
        );
    });
    await test('foreign cursor continuation is rejected without consuming the owner page', async () => {
        const f = cursorFixture({
            rows: [1, 2].map((value) => ({ key: Uint8Array.of(value), value: Uint8Array.of(value) }))
        });
        const first = await f.worker.scanPage({ store: 'docs', maxRows: 1, maxBytes: 64 });
        await assert.rejects(
            f.worker.scanPage({ store: 'other', cursorId: first.cursorId, maxRows: 1, maxBytes: 64 }),
            { name: 'InvalidRangeError' }
        );
        assert.equal(f.counts.pages, 1);
        const second = await f.worker.scanPage({ store: 'docs', cursorId: first.cursorId, maxRows: 1, maxBytes: 64 });
        assert.equal(protocol.unpackPackedScanRows(second.rows.bytes)[0].key[0], 2);
    });
    await test('compressed cursor pages respect expanded byte budgets', async () => {
        const raw = new Uint8Array(2048).fill(1);
        const value = await compression.encodeStoreValueRecord(raw, 'snappy');
        const f = cursorFixture({ kind: 'snappy', rows: [1, 2].map((key) => ({ key: Uint8Array.of(key), value })) });
        const first = await f.worker.scanPage({ store: 'docs', maxRows: 64, maxBytes: 4 + 8 + 1 + raw.byteLength });
        assert.deepEqual(first.rows[0].value, raw);
        assert.equal(first.rows.length, 1);
        assert.equal(first.bytes, 2061);
        await f.worker.closeCursor(first.cursorId);
        return { rawBytes: raw.byteLength, storedBytes: value.byteLength, logicalPageBudget: first.bytes };
    });
    await test('cursor budget and compressed corruption errors release their snapshot', async () => {
        const raw = new Uint8Array(2048).fill(1);
        const value = await compression.encodeStoreValueRecord(raw, 'snappy');
        for (const corrupt of [false, true]) {
            const row = value.slice();
            if (corrupt) row[row.byteLength - 1] ^= 1;
            const f = cursorFixture({ kind: 'snappy', rows: [{ key: Uint8Array.of(1), value: row }] });
            await assert.rejects(f.worker.scanPage({ store: 'docs', maxRows: 64, maxBytes: corrupt ? 4096 : 64 }), {
                name: corrupt ? 'CorruptionError' : 'ValueTooLargeError'
            });
            assert.equal(f.cursors.size + f.worker.primaryCursors.size + f.transactions.size, 0);
            assert.equal(f.counts.rollbacks, 1);
        }
    });
    await test('native size metadata keeps small indexed rows on the bounded bulk path', async () => {
        const f = fixture({ unique: false, nativeSizes: true });
        await f.worker.putMany(
            1,
            'docs',
            Array.from({ length: 100 }, (_, index) => [codec.u64Key(index), document('same')])
        );
        f.worker.txModes.set(1, 'readonly');
        f.counts.primaryReads = 0;
        const maxBytes = 8192;
        const page = await f.worker.scanByIndexPage(1, 'docs', 'email', {}, null, 100, maxBytes);
        assert.equal(page.rows.length, 100);
        assert.equal(f.counts.getManyCalls, 1);
        assert.equal(f.counts.primaryReads, 100);
        assert.ok(f.counts.lastBatchValues.every((value) => value === null));
        assert.ok(f.counts.batchLogicalBytes.every((size) => size <= maxBytes - 4));
        return {
            rows: 100,
            metadataCalls: f.counts.metadataCalls,
            bodyBatchCalls: f.counts.getManyCalls,
            materializedBytes: f.counts.batchLogicalBytes[0]
        };
    });
    for (const incompressible of [false, true]) {
        for (const reverse of [false, true]) {
            await test(`indexed byte pages retain the unreturned candidate: incompressible=${incompressible}, reverse=${reverse}`, async () => {
                const f = fixture({ kind: 'snappy', unique: false, nativeSizes: true });
                let padding = 'x'.repeat(1024 * 1024);
                if (incompressible) {
                    const noise = Buffer.alloc(768 * 1024);
                    let state = 1;
                    for (let index = 0; index < noise.length; index++) {
                        state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
                        noise[index] = state >>> 24;
                    }
                    padding = noise.toString('base64');
                }
                const raw = document('same', padding);
                await f.worker.putMany(
                    1,
                    'docs',
                    [1, 2, 3].map((index) => [codec.u64Key(index), raw])
                );
                assert.equal(f.stores.get('docs').get(hex(codec.u64Key(1))).value[9], incompressible ? 0 : 3);
                f.worker.txModes.set(1, 'readonly');
                f.counts.primaryReads = 0;
                const maxBytes = 4 + 8 + 8 + raw.byteLength + 18;
                let cursor = null;
                const keys = [];
                for (let pageIndex = 0; pageIndex < 4; pageIndex++) {
                    const beforeReads = f.counts.primaryReads;
                    const page = await f.worker.scanByIndexPage(1, 'docs', 'email', { reverse }, cursor, 256, maxBytes);
                    assert.ok(page.rows.length <= 1);
                    assert.ok(
                        f.counts.primaryReads - beforeReads <= 2,
                        'more than one extra candidate was materialized'
                    );
                    assert.ok(
                        4 + page.rows.reduce((sum, row) => sum + 8 + row.key.byteLength + row.value.byteLength, 0) <=
                            maxBytes
                    );
                    keys.push(
                        ...page.rows.map((row) =>
                            Number(new DataView(row.key.buffer, row.key.byteOffset, 8).getBigUint64(0, false))
                        )
                    );
                    if (page.cursor === null) break;
                    cursor = page.cursor;
                }
                assert.deepEqual(keys, reverse ? [3, 2, 1] : [1, 2, 3]);
                assert.equal(f.counts.getManyCalls, 0);
                return { bytesPerRow: raw.byteLength, maxBytes, maxPrimaryReadsPerPage: 2 };
            });
        }
    }
    await test('indexed default byte budget does not materialize all large primary bodies', async () => {
        const f = fixture({ kind: 'snappy', unique: false });
        const raw = document('same', 'x'.repeat(5 * 1024 * 1024));
        await f.worker.putMany(
            1,
            'docs',
            [1, 2, 3].map((index) => [codec.u64Key(index), raw])
        );
        f.worker.txModes.set(1, 'readonly');
        f.counts.primaryReads = 0;
        const page = await f.worker.scanByIndexPage(1, 'docs', 'email', {}, null, 256);
        assert.equal(page.rows.length, 1);
        assert.equal(f.counts.primaryReads, 2);
        assert.notEqual(page.cursor, null);
    });
    await test('indexed oversized first rows and invalid byte budgets fail explicitly', async () => {
        const f = fixture({ unique: false, nativeSizes: true });
        const raw = document('same', 'x'.repeat(4096));
        await f.worker.putMany(1, 'docs', [[codec.u64Key(1), raw]]);
        f.worker.txModes.set(1, 'readonly');
        await assert.rejects(f.worker.scanByIndexPage(1, 'docs', 'email', {}, null, 256, 64), {
            name: 'ValueTooLargeError'
        });
        for (const maxBytes of [0, 3, NaN, 4.5, 0x1_0000_0000]) {
            await assert.rejects(f.worker.scanByIndexPage(1, 'docs', 'email', {}, null, 256, maxBytes), {
                name: 'InvalidRangeError'
            });
        }
    });
    await test('native metadata failures preserve earlier document error order', async () => {
        const f = fixture({ unique: false, nativeSizes: true });
        await f.worker.putMany(
            1,
            'docs',
            [1, 2].map((index) => [codec.u64Key(index), document('same')])
        );
        f.stores.get('docs').get(hex(codec.u64Key(1))).value = Uint8Array.of(255);
        f.worker.txModes.set(1, 'readonly');
        f.worker.engine.get_many_value_sizes = () => {
            throw named('CorruptionError');
        };
        await assert.rejects(f.worker.scanByIndexPage(1, 'docs', 'email', {}, null, 256), {
            name: 'SerializationError'
        });
        assert.equal(f.counts.getManyCalls, 0);
    });
    await test('bounded native primary outcome mismatches remain explicit errors', async () => {
        const f = fixture({ unique: false, nativeSizes: true });
        await f.worker.putMany(
            1,
            'docs',
            [1, 2].map((index) => [codec.u64Key(index), document('same')])
        );
        f.worker.txModes.set(1, 'readonly');
        f.worker.engine.get_many = () => [];
        await assert.rejects(f.worker.scanByIndexPage(1, 'docs', 'email', {}, null, 256), { name: 'InternalError' });
    });
    await test('stale oversized documents do not consume the indexed page byte budget', async () => {
        const f = fixture({ unique: false, nativeSizes: true });
        await f.worker.putMany(1, 'docs', [
            [codec.u64Key(1), document('a', 'x'.repeat(4096))],
            [codec.u64Key(2), document('z')]
        ]);
        f.stores.get('docs').get(hex(codec.u64Key(1))).value = document('changed', 'x'.repeat(4096));
        const page = await f.worker.scanByIndexPage(1, 'docs', 'email', {}, null, 256, 128);
        assert.equal(page.rows.length, 1);
        assert.equal(codec.jsonDecode(page.rows[0].value).email, 'z');
        assert.equal(f.stores.get(f.defs[0].internalStore).size, 1);
    });
    await test('SQL-style one-row demand stops before corrupt later primary bodies', async () => {
        const f = fixture({ unique: false, nativeSizes: true });
        await f.worker.putMany(
            1,
            'docs',
            [1, 2, 3].map((index) => [codec.u64Key(index), document('same')])
        );
        f.worker.txModes.set(1, 'readonly');
        f.counts.primaryReads = 0;
        f.failRead((_store, key) => hex(key) === hex(codec.u64Key(2)));
        const page = await f.worker.scanByIndexPage(1, 'docs', 'email', {}, null, 1, 128);
        assert.equal(page.rows.length, 1);
        assert.equal(f.counts.primaryReads, 1);
        assert.equal(f.counts.getManyCalls + f.counts.metadataCalls, 0);
    });
    async function observeValidation(f, run) {
        const originalParse = JSON.parse;
        const originalResolve = f.worker.finishVisibleIndexedValue;
        const originalRead = f.worker.readStoreValue;
        assert.equal(typeof originalResolve, 'function');
        assert.equal(typeof originalRead, 'function');
        const beforeBodies = f.counts.primaryBodyReads;
        let jsonParses = 0;
        let valueResolutions = 0;
        JSON.parse = function (...args) {
            jsonParses++;
            return originalParse.apply(this, args);
        };
        f.worker.finishVisibleIndexedValue = function (...args) {
            valueResolutions++;
            return originalResolve.apply(this, args);
        };
        f.worker.readStoreValue = async function (...args) {
            const value = await originalRead.apply(this, args);
            if (value !== null) valueResolutions++;
            return value;
        };
        try {
            await run();
            return { bodyReads: f.counts.primaryBodyReads - beforeBodies, jsonParses, valueResolutions };
        } finally {
            JSON.parse = originalParse;
            f.worker.finishVisibleIndexedValue = originalResolve;
            f.worker.readStoreValue = originalRead;
        }
    }
    await test('matching native revisions reject unique conflicts without primary bodies or JSON', async () => {
        const f = fixture({ kind: 'snappy', unique: true, nativeRevisions: true });
        await f.worker.put(1, 'docs', codec.u64Key(1), document('same', 'x'.repeat(4096)));
        const evidence = await observeValidation(f, async () => {
            await assert.rejects(
                f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('same'), codec.u64Key(2)),
                { name: 'UniqueIndexConstraintError' }
            );
        });
        assert.deepEqual(evidence, { bodyReads: 0, jsonParses: 0, valueResolutions: 0 });
        return { ...evidence, metadataCalls: f.counts.stateCalls };
    });
    await test('native key-only index pages do not materialize raw index payload bodies', async () => {
        const f = fixture({ unique: true, nativeRevisions: true, nativeKeyScan: true });
        const key = codec.u64Key(1);
        await f.worker.put(1, 'docs', key, document('same'));
        const physical = indexing.encodeIndexEntryKey(codec.indexKey('same'), key);
        f.rawWrite(f.defs[0].internalStore, physical, new Uint8Array(2 * 1024 * 1024));
        f.counts.keyPages = 0;
        f.counts.keyCursorCloses = 0;
        const evidence = await observeValidation(f, () =>
            assert.rejects(
                f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('same'), codec.u64Key(2)),
                { name: 'UniqueIndexConstraintError' }
            )
        );
        assert.deepEqual(evidence, { bodyReads: 1, jsonParses: 1, valueResolutions: 1 });
        assert.equal(f.counts.indexBodyBytes, 0);
        assert.equal(f.counts.keyPages, 1);
        assert.equal(f.counts.keyCursorCloses, 1);
        return { indexBodyBytes: 0, keyPages: 1, closedCursors: 1 };
    });
    await test('unchanged managed index keys refresh native proof after scalar and duplicate bulk writes', async () => {
        const f = fixture({ unique: true, nativeRevisions: true });
        const key = codec.u64Key(1);
        const physical = indexing.encodeIndexEntryKey(codec.indexKey('same'), key);
        await f.worker.put(1, 'docs', key, document('same', 'first'));
        const old = f.stores.get('docs').get(hex(key)).revision;
        await f.worker.put(1, 'docs', key, document('same', 'second'));
        await f.worker.putMany(1, 'docs', [
            [key, document('same', 'third')],
            [key, document('same', 'last')]
        ]);
        const primary = f.stores.get('docs').get(hex(key));
        const index = f.stores.get(f.defs[0].internalStore).get(hex(physical));
        assert.notDeepEqual(primary.revision, old);
        assert.deepEqual(index.revision, primary.revision);
        assert.equal(codec.jsonDecode(primary.value).padding, 'last');
        const evidence = await observeValidation(f, () =>
            assert.rejects(
                f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('same'), codec.u64Key(2)),
                { name: 'UniqueIndexConstraintError' }
            )
        );
        assert.deepEqual(evidence, { bodyReads: 0, jsonParses: 0, valueResolutions: 0 });
        assert.equal(f.counts.indexedCalls, 4);
    });
    await test('scalar managed index failures preserve the changed primary without a completed-row event', async () => {
        const f = fixture({ unique: false, nativeRevisions: true });
        const key = codec.u64Key(1);
        f.failIndex(() => true);
        await assert.rejects(f.worker.put(1, 'docs', key, document('same')), { name: 'CorruptionError' });
        assert.equal(f.counts.indexedCalls, 1);
        assert.equal(f.counts.scalarCalls, 0);
        assert.equal(f.stores.get('docs').size, 1);
        assert.equal(f.stores.get(f.defs[0].internalStore).size, 0);
        assert.deepEqual(f.events(), []);
    });
    await test('matching readonly native proofs avoid JSON validation while returning bounded bodies', async () => {
        const f = fixture({ kind: 'snappy', unique: false, nativeRevisions: true });
        await f.worker.putMany(
            1,
            'docs',
            [1, 2, 3].map((id) => [codec.u64Key(id), document('same', 'x'.repeat(4096))])
        );
        f.snapshot(2);
        const evidence = await observeValidation(f, async () => {
            const page = await f.worker.scanByIndexPage(2, 'docs', 'email', {}, null, 3, 14000);
            assert.equal(page.rows.length, 3);
            assert.ok(page.rows.every((row) => row.value.byteLength > 4096));
        });
        assert.deepEqual(evidence, { bodyReads: 3, jsonParses: 0, valueResolutions: 3 });
        assert.equal(f.counts.getManyCalls, 1);
        return evidence;
    });
    await test('raw non-index primary rewrites require JSON fallback and retain the valid index entry', async () => {
        const f = fixture({ unique: true, nativeRevisions: true });
        const key = codec.u64Key(1);
        await f.worker.put(1, 'docs', key, document('same', 'managed'));
        f.rawWrite('docs', key, document('same', 'raw'));
        const evidence = await observeValidation(f, () =>
            assert.rejects(
                f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('same'), codec.u64Key(2)),
                { name: 'UniqueIndexConstraintError' }
            )
        );
        assert.deepEqual(evidence, { bodyReads: 1, jsonParses: 1, valueResolutions: 1 });
        assert.equal(f.stores.get(f.defs[0].internalStore).size, 1);
    });
    await test('raw index bytes cannot forge a matching native revision proof', async () => {
        const f = fixture({ unique: true, nativeRevisions: true });
        const key = codec.u64Key(1);
        await f.worker.put(1, 'docs', key, document('actual'));
        const revision = f.stores.get('docs').get(hex(key)).revision;
        const forged = new Uint8Array(24);
        forged.set(codec.utf8Encode('IDXREV01'));
        new DataView(forged.buffer).setBigUint64(8, revision.epoch, true);
        new DataView(forged.buffer).setBigUint64(16, revision.ordinal, true);
        const physical = indexing.encodeIndexEntryKey(codec.indexKey('forged'), key);
        f.rawWrite(f.defs[0].internalStore, physical, forged);
        assert.notDeepEqual(f.stores.get(f.defs[0].internalStore).get(hex(physical)).revision, revision);
        const evidence = await observeValidation(f, () =>
            f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('forged'), codec.u64Key(2))
        );
        assert.deepEqual(evidence, { bodyReads: 1, jsonParses: 1, valueResolutions: 1 });
        assert.equal(f.stores.get(f.defs[0].internalStore).has(hex(physical)), false);
    });
    await test('raw indexed-field rewrites and unknown legacy proof use full stale validation', async () => {
        for (const legacy of [false, true]) {
            const f = fixture({ unique: true, nativeRevisions: true });
            const key = codec.u64Key(1);
            await f.worker.put(1, 'docs', key, document('before'));
            if (legacy) f.stores.get('docs').get(hex(key)).revision = null;
            else f.rawWrite('docs', key, document('after'));
            const evidence = await observeValidation(f, async () => {
                if (legacy)
                    await assert.rejects(
                        f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('before'), codec.u64Key(2)),
                        { name: 'UniqueIndexConstraintError' }
                    );
                else
                    await f.worker.assertUniqueIndexAvailability(
                        1,
                        f.defs[0],
                        codec.indexKey('before'),
                        codec.u64Key(2)
                    );
            });
            assert.deepEqual(evidence, { bodyReads: 1, jsonParses: 1, valueResolutions: 1 });
            assert.equal(f.stores.get(f.defs[0].internalStore).size, legacy ? 1 : 0);
        }
    });
    await test('native expiration metadata preserves writer TTL cleanup without document bodies', async () => {
        const f = fixture({ kind: 'snappy', unique: true, nativeRevisions: true });
        const key = codec.u64Key(1);
        await f.worker.put(1, 'docs', key, document('same', 'x'.repeat(4096)), { ttl: 5 });
        f.setNow(1006);
        const evidence = await observeValidation(f, () =>
            f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('same'), codec.u64Key(2))
        );
        assert.deepEqual(evidence, { bodyReads: 0, jsonParses: 0, valueResolutions: 0 });
        assert.equal(f.stores.get('docs').size, 0);
        assert.equal(f.stores.get(f.defs[0].internalStore).size, 0);
        assert.equal(f.counts.expiryStages, 1);
    });
    await test('readonly expiration metadata skips bodies and does not stage TTL or stale cleanup', async () => {
        const f = fixture({ unique: false, nativeRevisions: true });
        await f.worker.putMany(
            1,
            'docs',
            [1, 2].map((id) => [codec.u64Key(id), document('same')]),
            { ttl: 5 }
        );
        f.snapshot(2);
        f.setNow(1006);
        const evidence = await observeValidation(f, async () => {
            assert.deepEqual(await f.worker.scanByIndexPage(2, 'docs', 'email', {}, null, 10), {
                rows: [],
                cursor: null
            });
        });
        assert.deepEqual(evidence, { bodyReads: 0, jsonParses: 0, valueResolutions: 0 });
        assert.equal(f.counts.getManyCalls, 0);
        assert.equal(f.stores.get('docs').size, 2);
        assert.equal(f.stores.get(f.defs[0].internalStore).size, 2);
        assert.equal(f.counts.expiryStages, 0);
    });
    await test('old snapshot proof remains valid after current raw updates and rollback restores writer proof', async () => {
        const f = fixture({ unique: true, nativeRevisions: true });
        const key = codec.u64Key(1);
        await f.worker.put(1, 'docs', key, document('same', 'old'));
        f.snapshot(2);
        f.checkpointWriter();
        f.rawWrite('docs', key, document('same', 'new'));
        const old = await observeValidation(f, () =>
            assert.rejects(
                f.worker.assertUniqueIndexAvailability(2, f.defs[0], codec.indexKey('same'), codec.u64Key(2)),
                { name: 'UniqueIndexConstraintError' }
            )
        );
        assert.deepEqual(old, { bodyReads: 0, jsonParses: 0, valueResolutions: 0 });
        const current = await observeValidation(f, () =>
            assert.rejects(
                f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('same'), codec.u64Key(2)),
                { name: 'UniqueIndexConstraintError' }
            )
        );
        assert.equal(current.bodyReads, 1);
        await f.worker.rollback(1);
        f.worker.txModes.set(3, 'readwrite');
        const restored = await observeValidation(f, () =>
            assert.rejects(
                f.worker.assertUniqueIndexAvailability(3, f.defs[0], codec.indexKey('same'), codec.u64Key(2)),
                { name: 'UniqueIndexConstraintError' }
            )
        );
        assert.deepEqual(restored, { bodyReads: 0, jsonParses: 0, valueResolutions: 0 });
    });
    await test('reconcile stamps validated raw primary rows with checked native revisions', async () => {
        const f = fixture({ unique: true, nativeRevisions: true });
        const key = codec.u64Key(1);
        await f.worker.put(1, 'docs', key, document('before'));
        f.rawWrite('docs', key, document('after'));
        await f.worker.reconcileIndexes(1, indexing.toPublicIndexDefinitions(f.defs));
        assert.equal(f.counts.checkedStamps, 1);
        const physical = indexing.encodeIndexEntryKey(codec.indexKey('after'), key);
        assert.deepEqual(
            f.stores.get(f.defs[0].internalStore).get(hex(physical)).revision,
            f.stores.get('docs').get(hex(key)).revision
        );
        const evidence = await observeValidation(f, () =>
            assert.rejects(
                f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('after'), codec.u64Key(2)),
                { name: 'UniqueIndexConstraintError' }
            )
        );
        assert.deepEqual(evidence, { bodyReads: 0, jsonParses: 0, valueResolutions: 0 });
    });
    await test('checked reconcile refuses a primary revision change before the native stamp', async () => {
        const f = fixture({ unique: true, nativeRevisions: true });
        const key = codec.u64Key(1);
        await f.worker.put(1, 'docs', key, document('before'));
        const checked = f.worker.engine.put_index_entry_checked;
        f.worker.engine.put_index_entry_checked = (...args) => {
            f.rawWrite('docs', key, document('changed'));
            return checked(...args);
        };
        await assert.rejects(f.worker.reconcileIndexes(1, indexing.toPublicIndexDefinitions(f.defs)), {
            name: 'TransactionConflictError'
        });
        assert.equal(f.counts.checkedStamps, 0);
        assert.equal(f.stores.get(f.defs[0].internalStore).size, 0);
    });
    await test('invalid native proof packets fall back to document errors in row order', async () => {
        const f = fixture({ unique: true, nativeRevisions: true });
        const key = codec.u64Key(1);
        await f.worker.put(1, 'docs', key, document('same'));
        f.rawWrite('docs', key, Uint8Array.of(255));
        f.worker.engine.get_many_value_states = () => Uint8Array.of(1, 0, 0, 0);
        await assert.rejects(
            f.worker.assertUniqueIndexAvailability(1, f.defs[0], codec.indexKey('same'), codec.u64Key(2)),
            { name: 'SerializationError' }
        );
        assert.equal(f.counts.primaryBodyReads, 1);
    });
    return {
        passed: results.filter((result) => result.passed).length,
        failed: results.filter((result) => !result.passed).length,
        results
    };
}
