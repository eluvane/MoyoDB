import { expect, test } from '@playwright/test';
import type * as Records from '../src/records';
import type * as Codec from '../src/codec';
import type * as Cases from './record-work-cases';
import { prepareMoyoDbPage, uniqueDbName } from './support';

test('typed record codecs preserve ordered key and lossless JSON contracts', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async () => {
        const recordPath = '/src/records.ts';
        const codecPath = '/src/codec.ts';
        const casePath = '/tests/record-work-cases.ts';
        const [records, codec, cases] = await Promise.all([
            import(recordPath) as Promise<typeof Records>,
            import(codecPath) as Promise<typeof Codec>,
            import(casePath) as Promise<typeof Cases>
        ]);
        return cases.checkRecordWork(records, codec);
    });
    expect(result.results.filter((entry) => !entry.passed)).toEqual([]);
    expect(result.passed).toBe(14);
});

test('typed stores integrate transactions, ordered scans, persisted JSON and managed indexes', async ({ page }) => {
    const dbName = uniqueDbName('typed-records');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const path = '/src/records.ts';
        const records = (await import(path)) as typeof Records;
        type User = { name: string; email: string; org: string; age: number };
        const codecs = { key: records.keyCodecs.number, value: records.jsonRecordCodec<User>() };
        let db = await window.moyodb.openDB(name, {
            version: 1,
            indexes: [
                { store: 'users', name: 'byEmail', keyPath: 'email', unique: true },
                { store: 'users', name: 'byOrgAge', keyPath: ['org', 'age'] }
            ],
            migrate: async ({ transaction }) => {
                await transaction.createStore('users');
                await transaction.createStore('counters');
            }
        });
        try {
            const users = records.openRecordStore(db, 'users', codecs);
            const counters = records.openRecordStore(db, 'counters', {
                key: records.keyCodecs.string,
                value: records.jsonRecordCodec<number>()
            });
            await users.putMany([
                [7, { name: 'Bea', email: 'bea@example.test', org: 'acme', age: 31 }],
                [1, { name: 'Ada', email: 'ada@example.test', org: 'acme', age: 30 }],
                [12, { name: 'Cy', email: 'cy@example.test', org: 'beta', age: 29 }]
            ]);
            await counters.put('created', 3);
            const all = await users.scan();
            const bounded = await users.scan({ gt: 1, lte: 12, reverse: true, limit: 1 });
            const many = await users.getMany([12, 1, 99]);
            const email = await users.index('byEmail', records.keyCodecs.string).get('ada@example.test');
            const byOrgAge = users.index(
                'byOrgAge',
                records.compoundKeyCodec(records.keyCodecs.string, records.keyCodecs.number)
            );
            const orgRows = await byOrgAge.scan({ gte: ['acme', 30], lte: ['acme', 31] });
            const readonlyName = await users.transaction(
                'readonly',
                async (store) => (await store.get(1))?.name ?? null
            );
            let uncommitted: string | null = null;
            let rollbackError: string | null = null;
            try {
                await users.transaction('readwrite', async (store, transaction) => {
                    await store.put(1, { name: 'Changed', email: 'changed@example.test', org: 'acme', age: 30 });
                    uncommitted = (await store.get(1))?.name ?? null;
                    await records
                        .openRecordStore(transaction, 'counters', {
                            key: records.keyCodecs.string,
                            value: records.jsonRecordCodec<number>()
                        })
                        .put('created', 4);
                    throw new Error('cancel records');
                });
            } catch (error) {
                rollbackError = error instanceof Error ? error.message : String(error);
            }
            const afterRollback = await users.get(1);
            const counterAfterRollback = await counters.get('created');
            await records.withRecordTransaction(db, 'readwrite', async (transaction) => {
                const bound = records.openRecordStore(transaction, 'users', codecs);
                await bound.applyBatch([
                    { kind: 'delete', key: 12 },
                    { kind: 'put', key: 9, value: { name: 'Dee', email: 'dee@example.test', org: 'beta', age: 28 } }
                ]);
                await records
                    .openRecordStore(transaction, 'counters', {
                        key: records.keyCodecs.string,
                        value: records.jsonRecordCodec<number>()
                    })
                    .put('created', 4);
            });
            const raw = await db.get('users', window.moyodb.indexKey(1));
            const rawName = raw === null ? null : window.moyodb.jsonDecode<User>(raw).name;
            await db.close();
            db = await window.moyodb.openDB(name);
            const reopened = records.openRecordStore(db, 'users', codecs);
            return {
                all: all.map((row) => row.key),
                bounded: bounded.map((row) => row.key),
                many: many.map((value) => value?.name ?? null),
                email: email?.name,
                orgRows: orgRows.map((row) => [row.key, row.value.name]),
                readonlyName,
                uncommitted,
                rollbackError,
                afterRollback: afterRollback?.name,
                counterAfterRollback,
                rawName,
                reopened: (await reopened.scan()).map((row) => [row.key, row.value.name]),
                reopenedIndex: (await reopened.index('byEmail', records.keyCodecs.string).get('dee@example.test'))
                    ?.name,
                removed: await reopened.has(12),
                counter: await records
                    .openRecordStore(db, 'counters', {
                        key: records.keyCodecs.string,
                        value: records.jsonRecordCodec<number>()
                    })
                    .get('created')
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toEqual({
        all: [1, 7, 12],
        bounded: [12],
        many: ['Cy', 'Ada', null],
        email: 'Ada',
        orgRows: [
            [1, 'Ada'],
            [7, 'Bea']
        ],
        readonlyName: 'Ada',
        uncommitted: 'Changed',
        rollbackError: 'cancel records',
        afterRollback: 'Ada',
        counterAfterRollback: 3,
        rawName: 'Ada',
        reopened: [
            [1, 'Ada'],
            [7, 'Bea'],
            [9, 'Dee']
        ],
        reopenedIndex: 'Dee',
        removed: false,
        counter: 4
    });
});

test('typed bulk failures roll back constraints and reject invalid persisted records', async ({ page }) => {
    const dbName = uniqueDbName('typed-record-errors');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const path = '/src/records.ts';
        const records = (await import(path)) as typeof Records;
        const db = await window.moyodb.openDB(name, {
            version: 1,
            indexes: [{ store: 'users', name: 'byEmail', keyPath: 'email', unique: true }],
            migrate: async ({ transaction }) => {
                await transaction.createStore('users');
            }
        });
        try {
            const users = records.openRecordStore(db, 'users', {
                key: records.keyCodecs.string,
                value: records.jsonRecordCodec<{ email: string }>()
            });
            await users.put('existing', { email: 'same@example.test' });
            let constraintError: string | null = null;
            try {
                await users.putMany([
                    ['valid', { email: 'new@example.test' }],
                    ['invalid', { email: 'same@example.test' }]
                ]);
            } catch (error) {
                constraintError = error instanceof Error ? error.name : String(error);
            }
            const keys = (await users.scan()).map((row) => row.key);
            await db.put('users', window.moyodb.indexKey('corrupt'), window.moyodb.utf8Encode('{"value":1e400}'));
            let decodingError: string | null = null;
            try {
                await users.get('corrupt');
            } catch (error) {
                decodingError = error instanceof Error ? error.name : String(error);
            }
            const tupleCodecs = {
                key: records.compoundKeyCodec(records.keyCodecs.string, records.keyCodecs.number),
                value: records.jsonRecordCodec<string>()
            };
            const logs = await records.createRecordStore(db, 'logs', tupleCodecs);
            await logs.putMany([
                [['org', 2], 'two'],
                [['org', 1], 'one'],
                [['beta', 0], 'zero']
            ]);
            const bounded = await logs.scan({ gte: ['org', 1], lt: ['org', 3] });
            await logs.deleteMany([['org', 1]]);
            const final = await logs.scan();
            await logs.clear();
            return { constraintError, keys, decodingError, bounded, final, cleared: await logs.scan() };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toEqual({
        constraintError: 'UniqueIndexConstraintError',
        keys: ['existing'],
        decodingError: 'TypeError',
        bounded: [
            { key: ['org', 1], value: 'one' },
            { key: ['org', 2], value: 'two' }
        ],
        final: [
            { key: ['beta', 0], value: 'zero' },
            { key: ['org', 2], value: 'two' }
        ],
        cleared: []
    });
});
