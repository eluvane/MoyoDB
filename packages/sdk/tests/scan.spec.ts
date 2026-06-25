import { test, expect } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('scan returns lexicographic order and supports bounds', async ({ page }) => {
    const dbName = uniqueDbName('scan');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('c'), window.moyodb.utf8Encode('3'));
            await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
            await db.put('kv', window.moyodb.utf8Encode('b'), window.moyodb.utf8Encode('2'));
            const rows = await db.scan('kv', {
                gte: window.moyodb.utf8Encode('a'),
                lte: window.moyodb.utf8Encode('c')
            });
            return rows.map((row) => window.moyodb.utf8Decode(row.key));
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toEqual(['a', 'b', 'c']);
});
test('large value survives overflow pages', async ({ page }) => {
    const dbName = uniqueDbName('overflow');
    await prepareMoyoDbPage(page);
    const size = 24 * 1024;
    const roundtrip = await page.evaluate(
        async ({ name, size }) => {
            const db = await window.moyodb.openDB(name);
            try {
                await db.createStore('blob');
                const value = new Uint8Array(size);
                for (let i = 0; i < value.length; i += 1) {
                    value[i] = i % 251;
                }
                await db.put('blob', window.moyodb.utf8Encode('big'), value);
                await db.close();
                const reopened = await window.moyodb.openDB(name);
                try {
                    const loaded = await reopened.get('blob', window.moyodb.utf8Encode('big'));
                    return {
                        len: loaded?.length ?? 0,
                        first: loaded?.[0] ?? -1,
                        last: loaded?.[loaded.length - 1] ?? -1
                    };
                } finally {
                    await reopened.close();
                }
            } finally {
            }
        },
        { name: dbName, size }
    );
    expect(roundtrip.len).toBe(size);
    expect(roundtrip.first).toBe(0);
    expect(roundtrip.last).toBe((size - 1) % 251);
});
