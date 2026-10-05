import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('transaction put and putMany treat expired TTL entries as absent', async ({ page }) => {
    const dbName = uniqueDbName('ttl-hidden');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name: string) => {
        const db = await window.moyodb.openDB(name);
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('kv');
            const tx = await db.begin('readwrite');
            await tx.put('kv', encode('a'), encode('1'), { ttl: 0 });
            await tx.putMany(
                'kv',
                [
                    [encode('b'), encode('2')],
                    [encode('c'), encode('3')]
                ],
                { ttl: 0 }
            );
            await tx.put('kv', encode('live'), encode('ok'));
            await tx.commit();
            const ro = await db.begin('readonly');
            const values = await ro.getMany('kv', [encode('a'), encode('b'), encode('c'), encode('live')]);
            const rows = await ro.scan('kv');
            await ro.rollback();
            return {
                values: values.map((value) => (value === null ? null : decode(value))),
                rows: rows.map((row) => ({
                    key: decode(row.key),
                    value: decode(row.value)
                }))
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.values).toEqual([null, null, null, 'ok']);
    expect(result.rows).toEqual([{ key: 'live', value: 'ok' }]);
});
test('lazy cleanup does not clobber a fresh staged overwrite of an expired key', async ({ page }) => {
    const dbName = uniqueDbName('ttl-overlay');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name: string) => {
        const db = await window.moyodb.openDB(name);
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('kv');
            await db.put('kv', encode('a'), encode('stale'), { ttl: 0 });
            const tx = await db.begin('readwrite');
            await tx.put('kv', encode('a'), encode('fresh'));
            const rows = await tx.scan('kv');
            await tx.commit();
            const value = await db.get('kv', encode('a'));
            return {
                rows: rows.map((row) => ({ key: decode(row.key), value: decode(row.value) })),
                value: value === null ? null : decode(value)
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.rows).toEqual([{ key: 'a', value: 'fresh' }]);
    expect(result.value).toBe('fresh');
});
