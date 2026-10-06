import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { after, test } from 'node:test';
import ts from 'typescript';

const source = resolve(dirname(fileURLToPath(import.meta.url)), '../src');
const output = await mkdtemp(join(tmpdir(), 'moyo-snapshot-worker-'));
for (const name of ['worker', 'worker-protocol', 'indexing', 'codec', 'errors', 'internal', 'compression', 'snappy']) {
    const result = ts.transpileModule(await readFile(join(source, `${name}.ts`), 'utf8'), {
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
    });
    await writeFile(join(output, `${name}.mjs`), result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'"));
}
await writeFile(join(output, 'worker-server.mjs'), 'export function exposeWorkerApi() {}\n');
const { DbWorker } = await import(pathToFileURL(join(output, 'worker.mjs')).href);
const { normalizeIndexDefinitions, encodeIndexMetadataKey, encodeIndexMetadataValue } = await import(
    pathToFileURL(join(output, 'indexing.mjs')).href
);
after(() => rm(output, { recursive: true, force: true }));

const encode = (value) => new TextEncoder().encode(value);
const decode = (value) => new TextDecoder().decode(value);
const named = (name) => Object.assign(new Error(name), { name });
const compare = (left, right) => Buffer.compare(left, right);
const snapshotWithIndexes = (rows, indexes) => {
    const base = encode(JSON.stringify(rows));
    const manifest = encode(JSON.stringify(indexes));
    const snapshot = new Uint8Array(base.length + manifest.length + 11);
    snapshot.set(base);
    snapshot.set(manifest, base.length);
    new DataView(snapshot.buffer).setUint32(base.length + manifest.length, manifest.length, true);
    snapshot.set(encode('BDBIDX1'), base.length + manifest.length + 4);
    return snapshot;
};

function fixture({ swapFailure = null } = {}) {
    const oldDefs = normalizeIndexDefinitions([{ store: 'keep', name: 'byValue', keyPath: 'value' }]);
    const oldStores = new Map([
        ['keep', [{ key: encode('old'), value: encode('{"value":"kept"}') }]],
        [
            '__browserdb:indexes',
            [{ key: encodeIndexMetadataKey('keep', 'byValue'), value: encodeIndexMetadataValue(oldDefs[0]) }]
        ],
        [oldDefs[0].internalStore, [{ key: encode('old-index'), value: new Uint8Array() }]]
    ]);
    const generations = new Map();
    let active = null;
    let swaps = 0;
    let cleanup = 0;
    class Engine {
        stores = new Map();
        transactions = new Map();
        next = 1n;
        last = 8n;
        abandoned = false;
        async openGeneration(_name, generation) {
            this.generation = generation;
            generations.set(generation, this);
        }
        begin_tx() {
            const id = this.next++;
            this.transactions.set(id, structuredClone(this.stores));
            return id;
        }
        rollback_tx(id) {
            this.transactions.delete(id);
        }
        commit_tx(id) {
            this.stores = this.transactions.get(id);
            this.transactions.delete(id);
            return ++this.last;
        }
        import_snapshot(bytes) {
            this.stores = new Map([
                ['docs', JSON.parse(decode(bytes)).map(([key, value]) => ({ key: encode(key), value: encode(value) }))]
            ]);
            return ++this.last;
        }
        import_snapshot_into(target, bytes) {
            target.last = this.last;
            return target.import_snapshot(bytes);
        }
        list_store_configs() {
            return [...this.stores.keys()].map((name) => ({ name, flags: 0n }));
        }
        db(id) {
            return this.transactions.get(id) ?? this.stores;
        }
        rows(id, name) {
            const rows = this.db(id).get(name);
            if (!rows) throw named('StoreNotFoundError');
            return rows;
        }
        scan(id, name, range = {}) {
            let rows = this.rows(id, name).filter(
                ({ key }) =>
                    (!range.gt || compare(key, range.gt) > 0) &&
                    (!range.gte || compare(key, range.gte) >= 0) &&
                    (!range.lt || compare(key, range.lt) < 0) &&
                    (!range.lte || compare(key, range.lte) <= 0)
            );
            if (range.limit !== undefined) rows = rows.slice(0, range.limit);
            return structuredClone(rows);
        }
        get(id, name, key) {
            return this.rows(id, name).find((row) => compare(row.key, key) === 0)?.value ?? null;
        }
        create_store(id, name) {
            if (this.db(id).has(name)) throw named('StoreExistsError');
            this.db(id).set(name, []);
        }
        clear_store(id, name) {
            this.rows(id, name);
            this.db(id).set(name, []);
        }
        put(id, name, key, value) {
            this.rows(id, name).push({ key: key.slice(), value: value.slice() });
            this.rows(id, name).sort((left, right) => compare(left.key, right.key));
        }
        abandon() {
            this.abandoned = true;
        }
    }
    const original = new Engine();
    original.stores = structuredClone(oldStores);
    const wasm = {
        WasmEngine: Engine,
        dbDirectorySize: async () => 100,
        readActiveGeneration: async () => active,
        prepareRebuildTarget: async () => ({ generationName: 'staged' }),
        swapActiveGeneration: async (_name, name, expected) => {
            assert.equal(active, expected);
            swaps++;
            if (swapFailure === 'before') throw named('StorageError');
            active = name;
            if (swapFailure === 'after') throw named('StorageError');
        },
        cleanupInactiveEntries: async () => {
            cleanup++;
            for (const name of generations.keys()) if (name !== active) generations.delete(name);
        }
    };
    const worker = new DbWorker({ loadWasm: async () => wasm, persistence: { close() {} } });
    worker.dbName = 'snapshot-worker';
    worker.engine = original;
    worker.committedIndexes = oldDefs;
    return {
        worker,
        original,
        oldStores,
        generations,
        active: () => active,
        swaps: () => swaps,
        cleanup: () => cleanup
    };
}

for (const [name, rows, unique, errorName] of [
    ['invalid JSON', [['bad', 'not-json']], false, 'SerializationError'],
    [
        'duplicate unique key',
        [
            ['one', '{"value":"same"}'],
            ['two', '{"value":"same"}']
        ],
        true,
        'UniqueIndexConstraintError'
    ],
    ['oversized index key', [['long', JSON.stringify({ value: 'x'.repeat(1100) })]], false, 'KeyTooLargeError']
]) {
    test(`rejected snapshot with ${name} keeps the active data and indexes`, async () => {
        const f = fixture();
        await assert.rejects(
            f.worker.importSnapshot(
                snapshotWithIndexes(rows, [{ store: 'docs', name: 'byValue', keyPath: 'value', unique }])
            ),
            { name: errorName }
        );
        assert.deepEqual(f.original.stores, f.oldStores);
        assert.equal(f.worker.engine, f.original);
        assert.equal(f.active(), null);
        assert.equal(f.swaps(), 0);
        assert.deepEqual(await f.worker.getIndexes(), [
            { store: 'keep', name: 'byValue', keyPath: 'value', unique: false }
        ]);
    });
}

for (const swapFailure of ['before', 'after', null]) {
    test(`snapshot publication handles ${swapFailure ?? 'successful'} control write`, async () => {
        const f = fixture({ swapFailure });
        const snapshot = snapshotWithIndexes(
            [['new', '{"value":"new"}']],
            [{ store: 'docs', name: 'byValue', keyPath: 'value' }]
        );
        if (swapFailure === 'before') {
            await assert.rejects(f.worker.importSnapshot(snapshot), { name: 'StorageError' });
            assert.equal(f.worker.engine, f.original);
            assert.equal(f.active(), null);
        } else {
            await f.worker.importSnapshot(snapshot);
            assert.equal(f.active(), 'staged');
            assert.equal(f.worker.engine, f.generations.get('staged'));
            assert.equal(f.original.abandoned, true);
            assert.equal(f.worker.engine.stores.get('__browserdb:indexes').length, 1);
            assert.equal(
                f.worker.engine.stores.get(
                    normalizeIndexDefinitions([{ store: 'docs', name: 'byValue', keyPath: 'value' }])[0].internalStore
                ).length,
                1
            );
        }
        assert.deepEqual(f.original.stores, f.oldStores);
    });
}

test('u64 conversion rejects unsafe schema versions and feed IDs without rounding', async () => {
    const f = fixture();
    f.original.get_schema_version = () => BigInt(Number.MAX_SAFE_INTEGER) + 2n;
    assert.throws(() => f.worker.getVersion(), { name: 'SerializationError' });
    f.original.changes_since = () => ({ latestTxId: BigInt(Number.MAX_SAFE_INTEGER) + 1n, changes: [] });
    await assert.rejects(f.worker.changesSince(0), { name: 'SerializationError' });
    f.original.get_schema_version = () => BigInt(Number.MAX_SAFE_INTEGER);
    assert.equal(await f.worker.getVersion(), Number.MAX_SAFE_INTEGER);
    f.original.changes_since = () => ({ latestTxId: BigInt(Number.MAX_SAFE_INTEGER), changes: [] });
    assert.equal((await f.worker.changesSince(0)).latestTxId, Number.MAX_SAFE_INTEGER);
});
