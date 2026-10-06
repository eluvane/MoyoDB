import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import ts from 'typescript';

const modules = new Map();
async function moduleUrl(name) {
    if (modules.has(name)) return modules.get(name);
    const source = await readFile(new URL(`../src/${name}.ts`, import.meta.url), 'utf8');
    let code = ts.transpileModule(source, {
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
    }).outputText;
    for (const dependency of new Set(Array.from(code.matchAll(/from '\.\/([^']+)'/g), (match) => match[1]))) {
        code = code.replaceAll(`from './${dependency}'`, `from '${await moduleUrl(dependency)}'`);
    }
    const url = `data:text/javascript;base64,${Buffer.from(`${code}\n//# sourceURL=moyodb-test://${name}.ts`).toString('base64')}`;
    modules.set(name, url);
    return url;
}
const { createSqlClient } = await import(await moduleUrl('sql'));
const { indexKey, jsonDecode } = await import(await moduleUrl('codec'));
const protocol = await import(await moduleUrl('worker-protocol'));
const identity = (bytes) => Buffer.from(bytes).toString('hex');
const compare = (left, right) => Buffer.compare(left, right);
const inRange = (key, range) =>
    (!range.gt || compare(key, range.gt) > 0) &&
    (!range.gte || compare(key, range.gte) >= 0) &&
    (!range.lt || compare(key, range.lt) < 0) &&
    (!range.lte || compare(key, range.lte) <= 0);

function memoryDatabase(indexes = []) {
    const stores = new Map();
    const work = { primaryRows: 0, indexRows: 0, pages: 0, closes: 0, active: 0, cursors: 0, pageDemands: [] };
    const db = {
        async begin(mode) {
            work.active += 1;
            const cursors = new Map();
            let nextCursor = 1;
            let finished = false;
            const finish = async () => {
                assert.equal(finished, false);
                finished = true;
                work.active -= 1;
                work.cursors -= cursors.size;
                cursors.clear();
            };
            return {
                mode,
                listIndexes: async () => indexes,
                get: async (store, key) => {
                    if (!stores.has(store)) throw Object.assign(new Error(), { name: 'StoreNotFoundError' });
                    return stores.get(store).get(identity(key))?.value ?? null;
                },
                has: async (store, key) => stores.get(store)?.has(identity(key)) ?? false,
                createStore: async (store) => stores.set(store, new Map()),
                put: async (store, key, value) => stores.get(store).set(identity(key), { key, value }),
                putMany: async (store, entries) => {
                    for (const [key, value] of entries) stores.get(store).set(identity(key), { key, value });
                },
                async scanPage(store, range = {}, options) {
                    work.pages += 1;
                    work.pageDemands.push(options.maxRows);
                    let cursor = options.cursor;
                    let state = cursors.get(cursor);
                    if (!state) {
                        const rows = [...stores.get(store).values()]
                            .sort((left, right) => compare(left.key, right.key))
                            .filter((row) => inRange(row.key, range));
                        if (range.reverse) rows.reverse();
                        state = { rows, offset: 0 };
                        cursor = nextCursor++;
                        cursors.set(cursor, state);
                        work.cursors += 1;
                    }
                    const rows = state.rows.slice(state.offset, state.offset + options.maxRows);
                    work.primaryRows += rows.length;
                    state.offset += rows.length;
                    const done = state.offset === state.rows.length;
                    if (done) {
                        cursors.delete(cursor);
                        work.cursors -= 1;
                    }
                    return {
                        rows,
                        cursor: done ? undefined : cursor,
                        done,
                        bytes: rows.reduce((size, row) => size + 8 + row.key.length + row.value.length, 4)
                    };
                },
                async closeScanCursor(cursor) {
                    if (cursors.delete(cursor)) {
                        work.closes += 1;
                        work.cursors -= 1;
                    }
                },
                async *scanByIndex(store, name, range = {}, options) {
                    const definition = indexes.find((index) => index.store === store && index.name === name);
                    const rows = [...stores.get(store).values()]
                        .flatMap((row) => {
                            const document = jsonDecode(row.value);
                            if (!Object.hasOwn(document, definition.keyPath)) return [];
                            return [{ ...row, logical: indexKey(document[definition.keyPath]) }];
                        })
                        .filter((row) => inRange(row.logical, range))
                        .sort((left, right) => compare(left.logical, right.logical) || compare(left.key, right.key));
                    if (range.reverse) rows.reverse();
                    assert.ok(options.maxRows > 0 && options.maxRows <= 256);
                    for (const row of rows) {
                        work.indexRows += 1;
                        yield [row.key, row.value];
                    }
                },
                commit: finish,
                rollback: finish
            };
        }
    };
    return {
        db,
        work,
        reset() {
            Object.assign(work, { primaryRows: 0, indexRows: 0, pages: 0, closes: 0, pageDemands: [] });
        }
    };
}

async function fixture(indexes = []) {
    const memory = memoryDatabase(indexes);
    const sql = createSqlClient(memory.db);
    await sql.execute('CREATE TABLE rows (id INTEGER PRIMARY KEY, rank REAL, label TEXT NOT NULL, active BOOLEAN)');
    const parameters = [];
    const values = [];
    for (let id = 1; id <= 100; id += 1) {
        values.push('(?, ?, ?, ?)');
        parameters.push(id, id % 5 === 0 ? null : id % 4, `item-${String(101 - id).padStart(3, '0')}`, id % 10 === 0);
    }
    await sql.execute(`INSERT INTO rows VALUES ${values.join(',')}`, parameters);
    memory.reset();
    return { ...memory, sql };
}

test('SQL primary traversal stops at OFFSET plus accepted LIMIT matches', async () => {
    const { sql, work } = await fixture();
    const result = await sql.execute('SELECT id FROM rows WHERE active = ? ORDER BY id LIMIT ? OFFSET ?', [true, 2, 1]);
    assert.deepEqual(result.rows, [{ id: 20 }, { id: 30 }]);
    assert.equal(result.plan.sort, 'none');
    assert.equal(work.primaryRows, 30);
    assert.ok(work.pageDemands.every((count) => count <= 3));
    assert.equal(work.closes, 1);
    assert.equal(work.cursors, 0);
    assert.equal(work.active, 0);
});

test('SQL reverse primary range uses traversal order and LIMIT zero reads no pages', async () => {
    const { sql, work, reset } = await fixture();
    assert.deepEqual(await sql.query('SELECT id FROM rows WHERE id < ? ORDER BY id DESC LIMIT 2', [80]), [
        { id: 79 },
        { id: 78 }
    ]);
    assert.equal(work.primaryRows, 2);
    reset();
    assert.deepEqual(await sql.query('SELECT id FROM rows ORDER BY id LIMIT ?', [0]), []);
    assert.equal(work.pages, 0);
    await assert.rejects(sql.query('SELECT id FROM rows LIMIT ?', [1n]), { name: 'SqlTypeError' });
    assert.equal(work.active, 0);
});

test('SQL secondary ascending traversal keeps NULL and primary-key tie order', async () => {
    const { sql, work } = await fixture([{ store: 'rows', name: 'rank', keyPath: 'rank' }]);
    const result = await sql.execute('SELECT id, rank FROM rows ORDER BY rank LIMIT 3');
    assert.deepEqual(result.rows, [
        { id: 5, rank: null },
        { id: 10, rank: null },
        { id: 15, rank: null }
    ]);
    assert.equal(result.plan.sort, 'none');
    assert.equal(work.indexRows, 3);
    assert.equal(work.primaryRows, 0);
    assert.equal(work.active, 0);
});

test('SQL indexed LIMIT one consumes one candidate and releases its transaction', async () => {
    const { sql, work } = await fixture([{ store: 'rows', name: 'label', keyPath: 'label' }]);
    const result = await sql.execute("SELECT id FROM rows WHERE label >= 'item-001' LIMIT 1");
    assert.deepEqual(result.rows, [{ id: 100 }]);
    assert.equal(result.plan.access, 'index-range');
    assert.equal(result.plan.sort, 'none');
    assert.equal(work.indexRows, 1);
    assert.equal(work.primaryRows, 0);
    assert.equal(work.active, 0);
});

test('SQL secondary DESC with implicit primary ASC ties falls back to a complete sort', async () => {
    const { sql, work } = await fixture([{ store: 'rows', name: 'rank', keyPath: 'rank' }]);
    const result = await sql.execute('SELECT id, rank FROM rows WHERE rank >= 0 ORDER BY rank DESC LIMIT 3');
    assert.deepEqual(result.rows, [
        { id: 3, rank: 3 },
        { id: 7, rank: 3 },
        { id: 11, rank: 3 }
    ]);
    assert.equal(result.plan.sort, 'memory');
    assert.equal(work.indexRows, 80);
    assert.equal(work.active, 0);
});

test('SQL secondary reverse traversal uses explicit reverse primary ties', async () => {
    const { sql, work } = await fixture([{ store: 'rows', name: 'rank', keyPath: 'rank' }]);
    const result = await sql.execute('SELECT id, rank FROM rows ORDER BY rank DESC, id DESC LIMIT 3');
    assert.deepEqual(result.rows, [
        { id: 99, rank: 3 },
        { id: 91, rank: 3 },
        { id: 87, rank: 3 }
    ]);
    assert.equal(result.plan.sort, 'none');
    assert.equal(work.indexRows, 3);
});

test('SQL non-null unique secondary DESC has no ties and stops at LIMIT', async () => {
    const { sql, work } = await fixture([{ store: 'rows', name: 'label', keyPath: 'label', unique: true }]);
    const result = await sql.execute('SELECT id FROM rows ORDER BY label DESC LIMIT 2');
    assert.deepEqual(result.rows, [{ id: 1 }, { id: 2 }]);
    assert.equal(result.plan.sort, 'none');
    assert.equal(work.indexRows, 2);
});

test('SQL secondary LIMIT counts matches after residual filtering', async () => {
    const { sql, work } = await fixture([{ store: 'rows', name: 'rank', keyPath: 'rank' }]);
    assert.deepEqual(
        await sql.query("SELECT id FROM rows WHERE rank >= 0 AND label < 'item-030' ORDER BY rank LIMIT 1"),
        [{ id: 72 }]
    );
    assert.equal(work.indexRows, 15);
    assert.equal(work.active, 0);
});

test('SQL unsupported traversal order reads all candidates before LIMIT', async () => {
    const { sql, work } = await fixture();
    const result = await sql.execute('SELECT id FROM rows WHERE active = true OR rank IS NULL ORDER BY label LIMIT 2');
    assert.deepEqual(result.rows, [{ id: 100 }, { id: 95 }]);
    assert.equal(result.plan.sort, 'memory');
    assert.equal(work.primaryRows, 101);
    assert.equal(work.cursors, 0);
});

test('engine scan packets transfer directly and decode views with bounded validation', () => {
    const bytes = Uint8Array.of(1, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 9, 8, 7);
    const page = { rows: protocol.packedScanRows(bytes), cursorId: 4, done: false, bytes: bytes.length };
    const prepared = protocol.prepareWorkerResponsePayload('scanPage', page);
    assert.equal(prepared.result, page);
    assert.deepEqual(prepared.transfer, [bytes.buffer]);
    const decoded = protocol.decodeWorkerResponsePayload('scanPage', prepared.result);
    assert.deepEqual(decoded.rows[0], { key: Uint8Array.of(9), value: Uint8Array.of(8, 7) });
    assert.equal(decoded.rows[0].value.buffer, bytes.buffer);
    assert.throws(() => protocol.unpackPackedScanRows(bytes.subarray(0, 14)), /exceeds packet length/);
    assert.throws(() => protocol.unpackPackedScanRows(Uint8Array.of(255, 255, 255, 255)), /row count/);
    assert.throws(() => protocol.decodeWorkerResponsePayload('scanPage', { ...page, done: true }), /invalid scan page/);
});

test('packed secondary pages retain their buffer through the SharedWorker relay', () => {
    const prepared = protocol.prepareWorkerResponsePayload('scanByIndexPage', {
        rows: [{ key: Uint8Array.of(9), value: Uint8Array.of(8, 7) }],
        cursor: Uint8Array.of(6)
    });
    const relayed = protocol.prepareWorkerResponsePayload('scanByIndexPage', prepared.result);
    assert.equal(relayed.result, prepared.result);
    assert.deepEqual(relayed.transfer, prepared.transfer);
    const decoded = protocol.decodeWorkerResponsePayload('scanByIndexPage', relayed.result);
    assert.deepEqual(decoded.rows, [{ key: Uint8Array.of(9), value: Uint8Array.of(8, 7) }]);
    assert.equal(decoded.rows[0].value.buffer, prepared.result.rows.bytes.buffer);
});
