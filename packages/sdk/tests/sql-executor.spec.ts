import { expect, test } from '@playwright/test';
import type { DB, IndexDef } from '../src/types';
import { prepareMoyoDbPage, uniqueDbName } from './support';

test('SQL binds values, projects and orders rows, and implements NULL logic', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        const sql = window.moyodb.createSqlClient(db);
        try {
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
            const ordered = await sql.query(
                'SELECT name, age FROM people WHERE (age >= ? AND active = ?) OR age IS NULL ORDER BY age DESC, name ASC LIMIT ? OFFSET ?',
                [20, true, 2, 1]
            );
            const nullComparison = await sql.query('SELECT id FROM people WHERE age = NULL OR age != NULL');
            const unknownOrTrue = await sql.query('SELECT id FROM people WHERE age = NULL OR id = 2');
            const updated = await sql.execute('UPDATE people SET name = ?, age = ? WHERE id = ?', ['B', 25, 2]);
            const deleted = await sql.execute('DELETE FROM people WHERE age < ?', [25]);
            const final = await sql.query('SELECT id, name FROM people ORDER BY id');
            const errors: string[] = [];
            for (const [statement, parameters] of [
                ['INSERT INTO people VALUES (?, ?, ?, ?)', [4, undefined, 3, true]],
                ['SELECT id FROM people WHERE id = ?', []],
                ['SELECT missing FROM people', []],
                ['SELECT id FROM people LIMIT ?', [-1]],
                ['SELECT id FROM people LIMIT ?', [1n]],
                ['SELECT id FROM people WHERE name = ?', [42]]
            ] as Array<[string, unknown[]]>) {
                try {
                    await sql.execute(statement, parameters);
                } catch (error) {
                    errors.push(error instanceof Error ? error.name : String(error));
                }
            }
            return {
                ordered,
                nullComparison,
                unknownOrTrue,
                updated: updated.rowsAffected,
                deleted: deleted.rowsAffected,
                final,
                errors
            };
        } finally {
            await db.close();
        }
    }, uniqueDbName('sql-values'));
    expect(result.ordered).toEqual([
        { name: 'Cy', age: 20 },
        { name: 'Bea', age: null }
    ]);
    expect(result.nullComparison).toEqual([]);
    expect(result.unknownOrTrue).toEqual([{ id: 2 }]);
    expect(result.updated).toBe(1);
    expect(result.deleted).toBe(1);
    expect(result.final).toEqual([
        { id: 1, name: 'Ada' },
        { id: 2, name: 'B' }
    ]);
    expect(result.errors).toHaveLength(6);
});

test('SQL persists full signed integers, blobs, schema, and generated row identifiers after reopen', async ({
    page
}) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        let db = await window.moyodb.openDB(name);
        let sql = window.moyodb.createSqlClient(db);
        try {
            await sql.execute(
                'CREATE TABLE values_table (id INTEGER PRIMARY KEY, payload BLOB NOT NULL, score REAL, enabled BOOLEAN)'
            );
            await sql.execute('INSERT INTO values_table VALUES (?, ?, ?, ?), (?, ?, ?, ?), (?, ?, ?, ?)', [
                -9223372036854775808n,
                Uint8Array.of(0, 255),
                1.5,
                false,
                0,
                Uint8Array.of(1),
                null,
                true,
                9223372036854775807n,
                new Uint8Array(),
                2.5,
                null
            ]);
            await sql.execute('CREATE TABLE events (message TEXT NOT NULL, optional TEXT)');
            await sql.execute('INSERT INTO events (message) VALUES (?), (?)', ['first', 'second']);
            await sql.execute('DELETE FROM events WHERE message = ?', ['first']);
            await db.close();
            db = await window.moyodb.openDB(name);
            sql = window.moyodb.createSqlClient(db);
            await sql.execute('INSERT INTO events (message) VALUES (?)', ['third']);
            const values = await sql.query('SELECT * FROM values_table ORDER BY id');
            const range = await sql.execute('SELECT id FROM values_table WHERE id >= ? AND id < ? ORDER BY id DESC', [
                -9223372036854775808n,
                9223372036854775807n
            ]);
            const events = await sql.query('SELECT * FROM events');
            const rawKeys = (await db.scan('events'))
                .filter(({ key }) => key.length !== 1 || key[0] !== 255)
                .map(({ key }) => Array.from(key));
            const markerPresent = (await db.get('events', Uint8Array.of(255))) !== null;
            const exact = await sql.explain('SELECT * FROM values_table WHERE id = ?', [9223372036854775807n]);
            const fractional = await sql.execute('SELECT id FROM values_table WHERE id > ?', [-0.5]);
            return {
                values: values.map((row) => ({
                    ...row,
                    id: String(row.id),
                    payload: Array.from(row.payload as Uint8Array)
                })),
                range: range.rows.map((row) => String(row.id)),
                rangeAccess: range.plan?.access,
                events,
                rawKeys,
                markerPresent,
                exact,
                fractional: fractional.rows.map((row) => String(row.id)),
                fractionalAccess: fractional.plan?.access
            };
        } finally {
            await db.close();
        }
    }, uniqueDbName('sql-reopen'));
    expect(result.values).toEqual([
        { id: '-9223372036854775808', payload: [0, 255], score: 1.5, enabled: false },
        { id: '0', payload: [1], score: null, enabled: true },
        { id: '9223372036854775807', payload: [], score: 2.5, enabled: null }
    ]);
    expect(result.range).toEqual(['0', '-9223372036854775808']);
    expect(result.rangeAccess).toBe('primary-key-range');
    expect(result.exact.access).toBe('primary-key-lookup');
    expect(result.events).toEqual([
        { message: 'second', optional: null },
        { message: 'third', optional: null }
    ]);
    expect(result.rawKeys).toEqual([
        [0, 0, 0, 0, 0, 0, 0, 2],
        [0, 0, 0, 0, 0, 0, 0, 3]
    ]);
    expect(result.markerPresent).toBe(true);
    expect(result.fractional).toEqual(['0', '9223372036854775807']);
    expect(result.fractionalAccess).toBe('table-scan');
});

test('SQL rejects primary and nullability conflicts atomically', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        const sql = window.moyodb.createSqlClient(db);
        try {
            await sql.execute('CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)');
            await sql.execute("INSERT INTO users VALUES (1, 'one'), (2, 'two')");
            const errors: string[] = [];
            for (const statement of [
                "INSERT INTO users VALUES (3, 'three'), (1, 'duplicate')",
                "INSERT INTO users VALUES (3, 'three'), (3, 'duplicate')",
                "INSERT INTO users VALUES (3, 'three'), (4, NULL)",
                'UPDATE users SET id = 1 WHERE id = 2',
                'UPDATE users SET id = 3',
                'UPDATE users SET name = NULL WHERE id = 1'
            ]) {
                try {
                    await sql.execute(statement);
                } catch (error) {
                    errors.push(error instanceof Error ? error.name : String(error));
                }
            }
            const rows = await sql.query('SELECT * FROM users ORDER BY id');
            const moved = await sql.execute('UPDATE users SET id = 5 WHERE id = 2');
            const old = await sql.query('SELECT * FROM users WHERE id = 2');
            const newRow = await sql.query('SELECT * FROM users WHERE id = 5');
            return { errors, rows, moved: moved.rowsAffected, old, newRow, active: (await db.stats()).active_txns };
        } finally {
            await db.close();
        }
    }, uniqueDbName('sql-constraints'));
    expect(result.errors).toEqual(Array(6).fill('ConstraintError'));
    expect(result.rows).toEqual([
        { id: 1, name: 'one' },
        { id: 2, name: 'two' }
    ]);
    expect(result.moved).toBe(1);
    expect(result.old).toEqual([]);
    expect(result.newRow).toEqual([{ id: 5, name: 'two' }]);
    expect(result.active).toBe(0);
});

test('SQL chooses native secondary indexes and preserves them across changes and rollback', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const indexes: IndexDef[] = [
            { store: 'users', name: 'byEmail', keyPath: 'email', unique: true },
            { store: 'users', name: 'byAge', keyPath: 'age' },
            { store: 'users', name: 'byPayload', keyPath: 'payload' }
        ];
        let db = await window.moyodb.openDB(name, { version: 1, indexes, migrate: () => {} });
        let sql = window.moyodb.createSqlClient(db, { indexes });
        try {
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
            const exact = await sql.execute('SELECT id FROM users WHERE email = ?', ['b']);
            const range = await sql.execute('SELECT id FROM users WHERE email >= ? AND email < ? ORDER BY id', [
                'b',
                'z'
            ]);
            const integerPoint = await sql.execute('SELECT id FROM users WHERE age = ?', [9223372036854775807n]);
            const integerRange = await sql.execute('SELECT id FROM users WHERE age > ? ORDER BY id', [20]);
            const blobRange = await sql.execute('SELECT id FROM users WHERE payload >= ? ORDER BY id', [
                Uint8Array.of(1)
            ]);
            const disjunction = await sql.execute("SELECT id FROM users WHERE email = 'a' OR email = 'c' ORDER BY id");
            let uniqueError = '';
            try {
                await sql.execute('INSERT INTO users VALUES (?, ?, ?, ?), (?, ?, ?, ?)', [
                    4,
                    'd',
                    40,
                    Uint8Array.of(4),
                    5,
                    'b',
                    50,
                    Uint8Array.of(5)
                ]);
            } catch (error) {
                uniqueError = error instanceof Error ? error.name : String(error);
            }
            const rolledBack = await sql.query("SELECT id FROM users WHERE email = 'd'");
            await sql.execute("UPDATE users SET email = 'new', payload = ? WHERE id = 2", [Uint8Array.of(5)]);
            const oldEmail = await sql.query("SELECT id FROM users WHERE email = 'b'");
            const newEmail = await sql.query("SELECT id FROM users WHERE email = 'new'");
            await sql.execute('DELETE FROM users WHERE id = 1');
            await db.close();
            db = await window.moyodb.openDB(name);
            sql = window.moyodb.createSqlClient(db, { indexes });
            const reopened = await sql.query("SELECT id FROM users WHERE email = 'new'");
            const tx = await db.begin('readonly');
            let raw: unknown;
            try {
                raw = await tx.getByIndex('users', 'byEmail', window.moyodb.indexKey('new'));
            } finally {
                await tx.rollback();
            }
            return {
                exact,
                range,
                integerPoint,
                integerRange,
                blobRange,
                disjunction,
                uniqueError,
                rolledBack,
                oldEmail,
                newEmail,
                reopened,
                nativePresent: raw !== null
            };
        } finally {
            await db.close();
        }
    }, uniqueDbName('sql-indexes'));
    expect(result.exact.rows).toEqual([{ id: 2 }]);
    expect(result.exact.plan?.access).toBe('index-lookup');
    expect(result.range.rows).toEqual([{ id: 2 }, { id: 3 }]);
    expect(result.range.plan?.access).toBe('index-range');
    expect(result.integerPoint.rows).toEqual([{ id: 3 }]);
    expect(result.integerPoint.plan?.access).toBe('index-lookup');
    expect(result.integerRange.rows).toEqual([{ id: 2 }, { id: 3 }]);
    expect(result.integerRange.plan?.access).toBe('table-scan');
    expect(result.blobRange.rows).toEqual([{ id: 2 }, { id: 3 }]);
    expect(result.blobRange.plan?.access).toBe('index-range');
    expect(result.disjunction.rows).toEqual([{ id: 1 }, { id: 3 }]);
    expect(result.disjunction.plan?.access).toBe('table-scan');
    expect(result.uniqueError).toBe('UniqueIndexConstraintError');
    expect(result.rolledBack).toEqual([]);
    expect(result.oldEmail).toEqual([]);
    expect(result.newEmail).toEqual([{ id: 2 }]);
    expect(result.reopened).toEqual([{ id: 2 }]);
    expect(result.nativePresent).toBe(true);
});

test('SQL rolls back mutations after execution failure and surfaces transaction conflicts', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        const sql = window.moyodb.createSqlClient(db);
        try {
            await sql.execute('CREATE TABLE events (message TEXT NOT NULL)');
            const failingDb = {
                async begin(mode) {
                    const tx = await db.begin(mode);
                    return new Proxy(tx, {
                        get(target, property) {
                            if (property === 'putMany')
                                return async (store: string, entries: Array<[Uint8Array, Uint8Array]>) => {
                                    await target.put(store, entries[0][0], entries[0][1]);
                                    throw new Error('injected SQL write failure');
                                };
                            const value: unknown = Reflect.get(target, property);
                            return typeof value === 'function' ? (value.bind(target) as unknown) : value;
                        }
                    });
                }
            } as DB;
            let failure = '';
            try {
                await window.moyodb
                    .createSqlClient(failingDb)
                    .execute("INSERT INTO events VALUES ('first'), ('second')");
            } catch (error) {
                failure = error instanceof Error ? error.message : String(error);
            }
            const afterFailure = await sql.query('SELECT * FROM events');
            await sql.execute("INSERT INTO events VALUES ('kept')");
            const keys = (await db.scan('events'))
                .filter(({ key }) => key.length !== 1 || key[0] !== 255)
                .map(({ key }) => Array.from(key));
            const conflictingDb = {
                async begin(mode) {
                    const tx = await db.begin(mode);
                    return new Proxy(tx, {
                        get(target, property) {
                            if (property === 'commit')
                                return async () => {
                                    await sql.execute("INSERT INTO events VALUES ('competing')");
                                    await target.commit();
                                };
                            const value: unknown = Reflect.get(target, property);
                            return typeof value === 'function' ? (value.bind(target) as unknown) : value;
                        }
                    });
                }
            } as DB;
            let conflict = '';
            try {
                await window.moyodb.createSqlClient(conflictingDb).execute("INSERT INTO events VALUES ('conflicted')");
            } catch (error) {
                conflict = error instanceof Error ? error.name : String(error);
            }
            const rows = await sql.query('SELECT * FROM events');
            const markerPresent = (await db.get('events', Uint8Array.of(255))) !== null;
            return {
                failure,
                afterFailure,
                keys,
                markerPresent,
                conflict,
                rows,
                active: (await db.stats()).active_txns
            };
        } finally {
            await db.close();
        }
    }, uniqueDbName('sql-failure'));
    expect(result.failure).toBe('injected SQL write failure');
    expect(result.afterFailure).toEqual([]);
    expect(result.keys).toEqual([[0, 0, 0, 0, 0, 0, 0, 1]]);
    expect(result.markerPresent).toBe(true);
    expect(result.conflict).toBe('TransactionConflictError');
    expect(result.rows).toEqual([{ message: 'kept' }, { message: 'competing' }]);
    expect(result.active).toBe(0);
});

test('SQL protects store ownership and validates persisted rows and metadata', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        const sql = window.moyodb.createSqlClient(db);
        try {
            await db.createStore('raw');
            await db.put('raw', Uint8Array.of(1), Uint8Array.of(7));
            let collision = '';
            try {
                await sql.execute('CREATE TABLE raw (id INTEGER PRIMARY KEY)');
            } catch (error) {
                collision = error instanceof Error ? error.name : String(error);
            }
            const before = await db.listStores();
            await sql.execute('DROP TABLE IF EXISTS raw');
            const raw = await db.get('raw', Uint8Array.of(1));
            await sql.execute('CREATE TABLE data (id INTEGER PRIMARY KEY, name TEXT)');
            await sql.execute("INSERT INTO data VALUES (1, 'ok')");
            const stored = await db.scan('data');
            await db.put('data', stored[0].key, window.moyodb.jsonEncode({ id: 2, name: 'wrong key' }));
            let rowError = '';
            try {
                await sql.query('SELECT * FROM data');
            } catch (error) {
                rowError = error instanceof Error ? error.name : String(error);
            }
            await db.put(
                '_moyodb_sql_catalog',
                window.moyodb.utf8Encode('table:data'),
                window.moyodb.jsonEncode({ version: 1, table: 'data', columns: [] })
            );
            let schemaError = '';
            try {
                await sql.query('SELECT * FROM data');
            } catch (error) {
                schemaError = error instanceof Error ? error.name : String(error);
            }
            return {
                collision,
                before,
                raw: raw ? Array.from(raw) : null,
                rowError,
                schemaError,
                active: (await db.stats()).active_txns
            };
        } finally {
            await db.close();
        }
    }, uniqueDbName('sql-corruption'));
    expect(result.collision).toBe('StoreExistsError');
    expect(result.before).toEqual(['raw']);
    expect(result.raw).toEqual([7]);
    expect(result.rowError).toBe('SerializationError');
    expect(result.schemaError).toBe('SqlSchemaError');
    expect(result.active).toBe(0);
});
