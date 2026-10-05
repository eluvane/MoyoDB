import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createSqlClient, indexKey, jsonEncode, openNodeDB, utf8Encode } from '@moyodb/sdk/node';

const temporaryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../../../.tmp');
await mkdir(temporaryRoot, { recursive: true });
const output = await mkdtemp(join(temporaryRoot, 'sql-executor-'));
const cleanupPath = relative(temporaryRoot, output);
if (!cleanupPath || cleanupPath.startsWith('..') || isAbsolute(cleanupPath)) {
    throw new Error('SQL test cleanup escaped its temporary root');
}
const results = [];
const handles = new Set();

async function open(name, options = {}) {
    const db = await openNodeDB(name, { directory: output, ...options });
    handles.add(db);
    return db;
}

async function check(name, run) {
    try {
        await run();
        results.push({ name, passed: true });
    } catch (error) {
        results.push({ name, passed: false, error: error instanceof Error ? error.stack : String(error) });
    } finally {
        for (const db of handles) await db.close();
        handles.clear();
    }
}

function wrapDatabase(db, overrides) {
    const methods = new Map(Object.entries(overrides));
    return {
        async begin(mode) {
            const tx = await db.begin(mode);
            return new Proxy(tx, {
                get(target, property) {
                    if (methods.has(property)) return methods.get(property)(target);
                    const value = Reflect.get(target, property);
                    return typeof value === 'function' ? value.bind(target) : value;
                }
            });
        }
    };
}

try {
    await check('native SQL parameters, ordering, projection, and three-valued NULL', async () => {
        const db = await open('values');
        const sql = createSqlClient(db);
        await sql.execute(
            'CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER, active BOOLEAN NOT NULL)'
        );
        await sql.execute('INSERT INTO people VALUES (?, ?, ?, ?), (?, ?, ?, ?), (?, ?, ?, ?)', [
            1,
            'Ada',
            31,
            true,
            2,
            'Bea',
            null,
            false,
            3,
            'Cy',
            20,
            true
        ]);
        assert.deepEqual(
            await sql.query(
                'SELECT name, age FROM people WHERE (age >= ? AND active = ?) OR age IS NULL ORDER BY age DESC, name LIMIT ? OFFSET ?',
                [20, true, 2, 1]
            ),
            [
                { name: 'Cy', age: 20 },
                { name: 'Bea', age: null }
            ]
        );
        assert.deepEqual(await sql.query('SELECT id FROM people WHERE age = NULL OR age != NULL'), []);
        assert.deepEqual(await sql.query('SELECT id FROM people WHERE age = NULL OR id = 2'), [{ id: 2 }]);
        assert.deepEqual(await sql.query('SELECT id FROM people WHERE age = NULL AND id = 2'), []);
        assert.equal(
            (await sql.execute('UPDATE people SET name = ?, age = ? WHERE id = ?', ['B', 25, 2])).rowsAffected,
            1
        );
        assert.equal((await sql.execute('DELETE FROM people WHERE age < ?', [25])).rowsAffected, 1);
        assert.deepEqual(await sql.query('SELECT id, name FROM people ORDER BY id'), [
            { id: 1, name: 'Ada' },
            { id: 2, name: 'B' }
        ]);
        await assert.rejects(sql.execute('INSERT INTO people VALUES (?, ?, ?, ?)', [4, undefined, 3, true]), {
            name: 'TypeError'
        });
        await assert.rejects(sql.query('SELECT id FROM people WHERE id = ?', []), { name: 'RangeError' });
        await assert.rejects(sql.query('SELECT missing FROM people'), { name: 'SqlSchemaError' });
        await assert.rejects(sql.query('SELECT id FROM people LIMIT ?', [-1]), { name: 'SqlTypeError' });
        await assert.rejects(sql.query('SELECT id FROM people LIMIT ?', [1n]), { name: 'SqlTypeError' });
        await assert.rejects(sql.query('SELECT id FROM people WHERE name = ?', [42]), { name: 'SqlTypeError' });
        await assert.rejects(sql.query('DELETE FROM people'), { name: 'SqlSchemaError' });
        assert.equal((await db.stats()).active_txns, 0);
    });

    await check('native SQL full signed64, BLOB, schema, and surrogate metadata reopen', async () => {
        let db = await open('reopen');
        let sql = createSqlClient(db);
        await sql.execute('CREATE TABLE values_table (id INTEGER PRIMARY KEY, payload BLOB NOT NULL)');
        await sql.execute('INSERT INTO values_table VALUES (?, ?), (?, ?), (?, ?)', [
            -9223372036854775808n,
            Uint8Array.of(0, 255),
            0,
            Uint8Array.of(1),
            9223372036854775807n,
            new Uint8Array()
        ]);
        await sql.execute('CREATE TABLE events (message TEXT NOT NULL, optional TEXT)');
        await sql.execute("INSERT INTO events (message) VALUES ('first'), ('second')");
        await sql.execute("DELETE FROM events WHERE message = 'first'");
        await db.close();
        db = await open('reopen');
        sql = createSqlClient(db);
        await sql.execute("INSERT INTO events (message) VALUES ('third')");
        assert.deepEqual(await sql.query('SELECT * FROM values_table ORDER BY id'), [
            { id: -9223372036854775808n, payload: Uint8Array.of(0, 255) },
            { id: 0, payload: Uint8Array.of(1) },
            { id: 9223372036854775807n, payload: new Uint8Array() }
        ]);
        const range = await sql.execute('SELECT id FROM values_table WHERE id >= ? AND id < ? ORDER BY id DESC', [
            -9223372036854775808n,
            9223372036854775807n
        ]);
        assert.deepEqual(range.rows, [{ id: 0 }, { id: -9223372036854775808n }]);
        assert.equal(range.plan.access, 'primary-key-range');
        assert.equal(
            (await sql.explain('SELECT * FROM values_table WHERE id = ?', [9223372036854775807n])).access,
            'primary-key-lookup'
        );
        const fractional = await sql.execute('SELECT id FROM values_table WHERE id > ?', [-0.5]);
        assert.deepEqual(fractional.rows, [{ id: 0 }, { id: 9223372036854775807n }]);
        assert.equal(fractional.plan.access, 'table-scan');
        assert.deepEqual(await sql.query('SELECT * FROM events'), [
            { message: 'second', optional: null },
            { message: 'third', optional: null }
        ]);
        assert.deepEqual(
            (await db.scan('events'))
                .filter(({ key }) => key.length !== 1 || key[0] !== 255)
                .map(({ key }) => Array.from(key)),
            [
                [0, 0, 0, 0, 0, 0, 0, 2],
                [0, 0, 0, 0, 0, 0, 0, 3]
            ]
        );
    });

    await check('native SQL primary and null constraints leave all rows unchanged', async () => {
        const db = await open('constraints');
        const sql = createSqlClient(db);
        await sql.execute('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)');
        await sql.execute("INSERT INTO users VALUES (1, 'one'), (2, 'two')");
        for (const statement of [
            "INSERT INTO users VALUES (3, 'three'), (1, 'duplicate')",
            "INSERT INTO users VALUES (3, 'three'), (3, 'duplicate')",
            "INSERT INTO users VALUES (3, 'three'), (4, NULL)",
            'UPDATE users SET id = 1 WHERE id = 2',
            'UPDATE users SET id = 3',
            'UPDATE users SET name = NULL WHERE id = 1'
        ])
            await assert.rejects(sql.execute(statement), { name: 'ConstraintError' });
        assert.deepEqual(await sql.query('SELECT * FROM users ORDER BY id'), [
            { id: 1, name: 'one' },
            { id: 2, name: 'two' }
        ]);
        await sql.execute('UPDATE users SET id = 5 WHERE id = 2');
        assert.deepEqual(await sql.query('SELECT * FROM users WHERE id = 2'), []);
        assert.deepEqual(await sql.query('SELECT * FROM users WHERE id = 5'), [{ id: 5, name: 'two' }]);
    });

    await check('native SQL discovers indexes and retains index consistency through writes and rollback', async () => {
        const indexes = [
            { store: 'users', name: 'byEmail', keyPath: 'email', unique: true },
            { store: 'users', name: 'byAge', keyPath: 'age' },
            { store: 'users', name: 'byPayload', keyPath: 'payload' }
        ];
        let db = await open('indexes', { version: 1, indexes, migrate: () => {} });
        let sql = createSqlClient(db);
        await sql.execute(
            'CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL, age INTEGER NOT NULL, payload BLOB NOT NULL)'
        );
        await sql.execute('INSERT INTO users VALUES (?, ?, ?, ?), (?, ?, ?, ?), (?, ?, ?, ?)', [
            1,
            'a',
            20,
            Uint8Array.of(0),
            2,
            'b',
            30,
            Uint8Array.of(1),
            3,
            'c',
            9223372036854775807n,
            Uint8Array.of(255)
        ]);
        const exact = await sql.execute("SELECT id FROM users WHERE email = 'b'");
        assert.deepEqual(exact.rows, [{ id: 2 }]);
        assert.equal(exact.plan.access, 'index-lookup');
        const range = await sql.execute("SELECT id FROM users WHERE email >= 'b' AND email < 'z' ORDER BY id");
        assert.deepEqual(range.rows, [{ id: 2 }, { id: 3 }]);
        assert.equal(range.plan.access, 'index-range');
        assert.equal(
            (await sql.explain('SELECT id FROM users WHERE age = ?', [9223372036854775807n])).access,
            'index-lookup'
        );
        assert.deepEqual(await sql.query('SELECT id FROM users WHERE age = ?', [9223372036854775807n]), [{ id: 3 }]);
        const integerRange = await sql.execute('SELECT id FROM users WHERE age > 20 ORDER BY id');
        assert.deepEqual(integerRange.rows, [{ id: 2 }, { id: 3 }]);
        assert.equal(integerRange.plan.access, 'table-scan');
        const blobRange = await sql.execute('SELECT id FROM users WHERE payload >= ? ORDER BY id', [Uint8Array.of(1)]);
        assert.deepEqual(blobRange.rows, [{ id: 2 }, { id: 3 }]);
        assert.equal(blobRange.plan.access, 'index-range');
        const disjunction = await sql.execute("SELECT id FROM users WHERE email = 'a' OR email = 'c' ORDER BY id");
        assert.deepEqual(disjunction.rows, [{ id: 1 }, { id: 3 }]);
        assert.equal(disjunction.plan.access, 'table-scan');
        await assert.rejects(
            sql.execute('INSERT INTO users VALUES (?, ?, ?, ?), (?, ?, ?, ?)', [
                4,
                'd',
                40,
                Uint8Array.of(4),
                5,
                'b',
                50,
                Uint8Array.of(5)
            ]),
            { name: 'UniqueIndexConstraintError' }
        );
        assert.deepEqual(await sql.query("SELECT id FROM users WHERE email = 'd'"), []);
        await sql.execute("UPDATE users SET email = 'new', payload = ? WHERE id = 2", [Uint8Array.of(5)]);
        assert.deepEqual(await sql.query("SELECT id FROM users WHERE email = 'b'"), []);
        assert.deepEqual(await sql.query("SELECT id FROM users WHERE email = 'new'"), [{ id: 2 }]);
        await sql.execute('DELETE FROM users WHERE id = 1');
        await db.close();
        db = await open('indexes');
        sql = createSqlClient(db);
        assert.deepEqual(await sql.query("SELECT id FROM users WHERE email = 'new'"), [{ id: 2 }]);
        const tx = await db.begin('readonly');
        try {
            assert.notEqual(await tx.getByIndex('users', 'byEmail', indexKey('new')), null);
        } finally {
            await tx.rollback();
        }
        await assert.rejects(
            createSqlClient(db, { indexes: [{ store: 'users', name: 'byEmail', keyPath: 'age', unique: true }] }).query(
                'SELECT * FROM users'
            ),
            { name: 'SqlSchemaError' }
        );
    });

    await check('native SQL execution failure rolls back row and surrogate metadata changes', async () => {
        const db = await open('failure');
        const sql = createSqlClient(db);
        await sql.execute('CREATE TABLE events (message TEXT NOT NULL)');
        const failingDb = wrapDatabase(db, {
            putMany: (tx) => async (store, entries) => {
                await tx.put(store, entries[0][0], entries[0][1]);
                throw new Error('injected SQL write failure');
            }
        });
        await assert.rejects(
            createSqlClient(failingDb).execute("INSERT INTO events VALUES ('first'), ('second')"),
            /injected SQL write failure/u
        );
        assert.deepEqual(await sql.query('SELECT * FROM events'), []);
        await sql.execute("INSERT INTO events VALUES ('kept')");
        assert.deepEqual(
            (await db.scan('events'))
                .filter(({ key }) => key.length !== 1 || key[0] !== 255)
                .map(({ key }) => Array.from(key)),
            [[0, 0, 0, 0, 0, 0, 0, 1]]
        );
        assert.equal((await db.stats()).active_txns, 0);
    });

    await check('native SQL flat dotted columns fall back from nested-path indexes', async () => {
        const db = await open('dotted-column', {
            version: 1,
            indexes: [{ store: 'items', name: 'byDotted', keyPath: 'a.b' }],
            migrate: () => {}
        });
        const sql = createSqlClient(db);
        await sql.execute('CREATE TABLE items (id INTEGER PRIMARY KEY, "a.b" TEXT NOT NULL)');
        await sql.execute('INSERT INTO items VALUES (1, ?)', ['flat']);
        const result = await sql.execute('SELECT id FROM items WHERE "a.b" = ?', ['flat']);
        assert.deepEqual(result.rows, [{ id: 1 }]);
        assert.equal(result.plan.access, 'table-scan');
        assert.equal((await sql.explain('SELECT * FROM items WHERE "a.b" = ?', ['flat'])).access, 'table-scan');
    });

    await check('native SQL commit conflicts do not overwrite the committed transaction', async () => {
        const db = await open('conflicts');
        const sql = createSqlClient(db);
        await sql.execute('CREATE TABLE events (message TEXT NOT NULL)');
        const conflictingDb = wrapDatabase(db, {
            commit: (tx) => async () => {
                await sql.execute("INSERT INTO events VALUES ('competing')");
                await tx.commit();
            }
        });
        await assert.rejects(createSqlClient(conflictingDb).execute("INSERT INTO events VALUES ('conflicted')"), {
            name: 'TransactionConflictError'
        });
        assert.deepEqual(await sql.query('SELECT * FROM events'), [{ message: 'competing' }]);
        await db.close();
        const reopened = await open('conflicts');
        assert.deepEqual(await createSqlClient(reopened).query('SELECT * FROM events'), [{ message: 'competing' }]);
        assert.equal((await reopened.stats()).active_txns, 0);
    });

    await check('native SQL protects non-SQL stores and reports corrupt persisted data', async () => {
        const db = await open('ownership');
        const sql = createSqlClient(db);
        await db.createStore('raw');
        await db.put('raw', Uint8Array.of(1), Uint8Array.of(7));
        await assert.rejects(sql.execute('CREATE TABLE raw (id INTEGER PRIMARY KEY)'), { name: 'StoreExistsError' });
        assert.deepEqual(await db.listStores(), ['raw']);
        await sql.execute('DROP TABLE IF EXISTS raw');
        assert.deepEqual(await db.get('raw', Uint8Array.of(1)), Uint8Array.of(7));
        await sql.execute('CREATE TABLE data (id INTEGER PRIMARY KEY, name TEXT)');
        await sql.execute("INSERT INTO data VALUES (1, 'ok')");
        const [{ key }] = await db.scan('data');
        await db.put('data', key, jsonEncode({ id: 2, name: 'wrong key' }));
        await assert.rejects(sql.query('SELECT * FROM data'), { name: 'SerializationError' });
        await db.put(
            '_moyodb_sql_catalog',
            utf8Encode('table:data'),
            jsonEncode({ version: 1, table: 'data', columns: [] })
        );
        await assert.rejects(sql.query('SELECT * FROM data'), { name: 'SqlSchemaError' });
        assert.equal((await db.stats()).active_txns, 0);
    });

    await check('native SQL rejects stale ownership after raw store recreation', async () => {
        const db = await open('recreated-ownership');
        const sql = createSqlClient(db);
        await sql.execute('CREATE TABLE reused (id INTEGER PRIMARY KEY, value TEXT)');
        const schemaBefore = await db.get('_moyodb_sql_catalog', utf8Encode('table:reused'));
        await db.dropStore('reused');
        await db.createStore('reused');
        await db.put('reused', Uint8Array.of(1), Uint8Array.of(7));
        for (const statement of [
            'DROP TABLE reused',
            'DROP TABLE IF EXISTS reused',
            'CREATE TABLE IF NOT EXISTS reused (id INTEGER PRIMARY KEY, value TEXT)',
            'INSERT INTO reused VALUES (1, NULL)',
            'UPDATE reused SET value = NULL',
            'DELETE FROM reused',
            'SELECT * FROM reused'
        ])
            await assert.rejects(sql.execute(statement), { name: 'SqlSchemaError' });
        await assert.rejects(sql.explain('SELECT * FROM reused'), { name: 'SqlSchemaError' });
        assert.deepEqual(await db.get('reused', Uint8Array.of(1)), Uint8Array.of(7));
        assert.deepEqual(await db.get('_moyodb_sql_catalog', utf8Encode('table:reused')), schemaBefore);
        assert.equal((await db.stats()).active_txns, 0);
    });

    await check('native SQL scan fallback, empty ranges, special names, and UTF8 collation', async () => {
        const db = await open('fallback', {
            version: 1,
            indexes: [{ store: 'items', name: 'compound', keyPath: ['id', 'name'] }],
            migrate: () => {}
        });
        const sql = createSqlClient(db);
        await assert.rejects(sql.execute('CREATE TABLE rejected ("__proto__" TEXT)'), { name: 'SqlSyntaxError' });
        await sql.execute('CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL, "select" TEXT)');
        await sql.execute('INSERT INTO items VALUES (?, ?, ?), (?, ?, ?)', [
            1,
            '\ue000',
            'own1',
            2,
            '\ud83d\ude00',
            'own2'
        ]);
        const scan = await sql.execute('SELECT "select", id FROM items WHERE name >= ? ORDER BY name', ['']);
        assert.equal(scan.plan.access, 'table-scan');
        assert.deepEqual(scan.rows, [
            { select: 'own1', id: 1 },
            { select: 'own2', id: 2 }
        ]);
        assert.deepEqual(await sql.query('SELECT id FROM items WHERE id > 2 AND id <= 2'), []);
        assert.deepEqual(await sql.query('SELECT id FROM items WHERE id > 2 AND id < 1'), []);
        assert.deepEqual(await sql.query('SELECT id FROM items LIMIT 0'), []);
        await sql.execute('CREATE TABLE IF NOT EXISTS items (unrelated TEXT)');
        await sql.execute('DROP TABLE items');
        await assert.rejects(sql.query('SELECT * FROM items'), { name: 'SqlSchemaError' });
        await sql.execute('DROP TABLE IF EXISTS items');
        await sql.execute('CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT NOT NULL)');
        await sql.execute("INSERT INTO items VALUES (1, 'new')");
        assert.deepEqual(await sql.query('SELECT * FROM items'), [{ id: 1, name: 'new' }]);
    });
    process.stdout.write(
        `${JSON.stringify({ passed: results.filter((result) => result.passed).length, failed: results.filter((result) => !result.passed).length, results }, null, 2)}\n`
    );
    if (results.some((result) => !result.passed)) process.exitCode = 1;
} finally {
    for (const db of handles) await db.close();
    await rm(output, { recursive: true, force: true });
}
