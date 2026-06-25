import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('transaction batch methods read and write ordered key sets', async ({ page }) => {
    const dbName = uniqueDbName('batch-methods');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name: string) => {
        const db = await window.moyodb.openDB(name);
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('kv');
            const tx = await db.begin('readwrite');
            await tx.putMany('kv', [
                [encode('a'), encode('1')],
                [encode('b'), encode('2')],
                [encode('c'), encode('3')]
            ]);
            const initialRead = await tx.getMany('kv', [encode('c'), encode('a'), encode('missing')]);
            await tx.deleteMany('kv', [encode('b'), encode('missing')]);
            await tx.applyBatch('kv', [
                { kind: 'delete', key: encode('a') },
                { kind: 'put', key: encode('c'), value: encode('30') },
                { kind: 'put', key: encode('d'), value: encode('4') }
            ]);
            await tx.commit();
            const ro = await db.begin('readonly');
            const finalRead = await ro.getMany('kv', [encode('a'), encode('b'), encode('c'), encode('d')]);
            const rows = await ro.scan('kv');
            await ro.rollback();
            return {
                initialRead: initialRead.map((value) => (value === null ? null : decode(value))),
                finalRead: finalRead.map((value) => (value === null ? null : decode(value))),
                rows: rows.map((row) => ({
                    key: decode(row.key),
                    value: decode(row.value)
                }))
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.initialRead).toEqual(['3', '1', null]);
    expect(result.finalRead).toEqual([null, null, '30', '4']);
    expect(result.rows).toEqual([
        { key: 'c', value: '30' },
        { key: 'd', value: '4' }
    ]);
});
test('applyBatch keeps committed prefix visible to subscriptions when a later op fails', async ({ page }) => {
    const dbName = uniqueDbName('batch-partial');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name: string) => {
        const db = await window.moyodb.openDB(name);
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('kv');
            const delivery = new Promise<{
                changes: Array<{
                    key: string;
                    kind: 'put' | 'delete';
                }>;
                txnId: number;
            }>((resolve) => {
                const unsubscribe = db.subscribe('kv', (_store, changes, txnId) => {
                    unsubscribe();
                    resolve({
                        changes: changes.map((change) => ({
                            key: decode(change.key),
                            kind: change.kind
                        })),
                        txnId
                    });
                });
            });
            const tx = await db.begin('readwrite');
            let errorName: string | null = null;
            try {
                await tx.applyBatch('kv', [
                    { kind: 'put', key: encode('a'), value: encode('1') },
                    { kind: 'put', key: new Uint8Array(1025), value: encode('boom') }
                ]);
            } catch (error) {
                errorName = error instanceof Error ? error.name : String(error);
            }
            await tx.commit();
            const event = await delivery;
            const ro = await db.begin('readonly');
            const values = await ro.getMany('kv', [encode('a'), encode('missing')]);
            await ro.rollback();
            return {
                errorName,
                event,
                values: values.map((value) => (value === null ? null : decode(value)))
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.errorName).toBe('KeyTooLargeError');
    expect(result.event.txnId).toBeGreaterThan(0);
    expect(result.event.changes).toEqual([{ key: 'a', kind: 'put' }]);
    expect(result.values).toEqual(['1', null]);
});

test('putMany matches repeated put for plain stores', async ({ page }) => {
    const dbName = uniqueDbName('putmany-match');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name: string) => {
        const db = await window.moyodb.openDB(name);
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('bulk');
            await db.createStore('single');
            const entries: Array<[Uint8Array, Uint8Array]> = [];
            for (let i = 0; i < 128; i += 1) {
                entries.push([encode(`k-${i.toString().padStart(3, '0')}`), encode(`v-${i}`)]);
            }
            const bulkTx = await db.begin('readwrite');
            await bulkTx.putMany('bulk', entries);
            await bulkTx.commit();
            for (const [key, value] of entries) {
                await db.put('single', key, value);
            }
            const ro = await db.begin('readonly');
            const bulkValues = await ro.getMany(
                'bulk',
                entries.map(([key]) => key)
            );
            const singleValues = await ro.getMany(
                'single',
                entries.map(([key]) => key)
            );
            await ro.rollback();
            return {
                bulkValues: bulkValues.map((value) => (value ? decode(value) : null)),
                singleValues: singleValues.map((value) => (value ? decode(value) : null))
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.bulkValues).toEqual(result.singleValues);
    expect(result.bulkValues).toHaveLength(128);
});

test('getMany matches repeated get after putMany', async ({ page }) => {
    const dbName = uniqueDbName('getmany-match');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name: string) => {
        const db = await window.moyodb.openDB(name);
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('kv');
            const entries: Array<[Uint8Array, Uint8Array]> = [
                [encode('a'), encode('1')],
                [encode('b'), encode('2')],
                [encode('c'), encode('3')]
            ];
            const tx = await db.begin('readwrite');
            await tx.putMany('kv', entries);
            await tx.commit();
            const ro = await db.begin('readonly');
            const keys = [encode('c'), encode('a'), encode('missing'), encode('b')];
            const many = await ro.getMany('kv', keys);
            const single = [];
            for (const key of keys) {
                single.push(await ro.get('kv', key));
            }
            await ro.rollback();
            return {
                many: many.map((value) => (value ? decode(value) : null)),
                single: single.map((value) => (value ? decode(value) : null))
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.many).toEqual(['3', '1', null, '2']);
    expect(result.many).toEqual(result.single);
});

test('db.getMany reads scoped batches in caller key order', async ({ page }) => {
    const dbName = uniqueDbName('db-getmany');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name: string) => {
        const db = await window.moyodb.openDB(name);
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('kv');
            const tx = await db.begin('readwrite');
            await tx.putMany('kv', [
                [encode('a'), encode('1')],
                [encode('b'), encode('2')],
                [encode('c'), encode('3')]
            ]);
            await tx.commit();
            const values = await db.getMany('kv', [encode('c'), encode('missing'), encode('a'), encode('b')]);
            return values.map((value) => (value ? decode(value) : null));
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toEqual(['3', null, '1', '2']);
});

test('putMany rollback removes a partially applied failing batch', async ({ page }) => {
    const dbName = uniqueDbName('putmany-rollback');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name: string) => {
        const db = await window.moyodb.openDB(name);
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('kv');
            const tx = await db.begin('readwrite');
            let errorName: string | null = null;
            try {
                await tx.putMany('kv', [
                    [encode('ok-before-error'), encode('1')],
                    [new Uint8Array(1025), encode('boom')],
                    [encode('never-applied'), encode('2')]
                ]);
            } catch (error) {
                errorName = error instanceof Error ? error.name : String(error);
            }
            await tx.rollback();
            const ro = await db.begin('readonly');
            const values = await ro.getMany('kv', [encode('ok-before-error'), encode('never-applied')]);
            await ro.rollback();
            return {
                errorName,
                values: values.map((value) => (value ? decode(value) : null))
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.errorName).toBe('KeyTooLargeError');
    expect(result.values).toEqual([null, null]);
});

test('worker bulk putMany roundtrip commits a large batch', async ({ page }) => {
    const dbName = uniqueDbName('putmany-worker-bulk');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name: string) => {
        const db = await window.moyodb.openDB(name);
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('kv');
            const entries: Array<[Uint8Array, Uint8Array]> = [];
            for (let i = 0; i < 2048; i += 1) {
                entries.push([encode(`bulk-${i.toString().padStart(4, '0')}`), encode(`value-${i}`)]);
            }
            const tx = await db.begin('readwrite');
            await tx.putMany('kv', entries);
            await tx.commit();
            const ro = await db.begin('readonly');
            const keys = [entries[0]![0], entries[1024]![0], entries[2047]![0]];
            const values = await ro.getMany('kv', keys);
            const rows = await ro.scan('kv');
            await ro.rollback();
            return {
                values: values.map((value) => (value ? decode(value) : null)),
                rowCount: rows.length
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.values).toEqual(['value-0', 'value-1024', 'value-2047']);
    expect(result.rowCount).toBe(2048);
});
