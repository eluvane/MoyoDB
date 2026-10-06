import { test, expect } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('primary cursors bound decoded pages and retain snapshots until close or cancellation', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        const observed = [];
        try {
            for (const compression of [false, 'snappy'] as const) {
                const store = compression === false ? 'raw' : 'compressed';
                await db.createStore(store, { compression });
                const tx = await db.begin('readwrite');
                await tx.putMany(
                    store,
                    Array.from({ length: 20 }, (_, index) => [
                        window.moyodb.utf8Encode(String(index).padStart(3, '0')),
                        new Uint8Array(4096).fill(index)
                    ])
                );
                await tx.commit();
                const range = { gte: window.moyodb.utf8Encode('004'), lte: window.moyodb.utf8Encode('010') };
                const first = await db.scanPage(store, range, { maxRows: 10, maxBytes: 5120 });
                await db.put(store, window.moyodb.utf8Encode('005'), new Uint8Array(4096).fill(99));
                const next = await db.scanPage(store, range, { cursor: first.cursor, maxRows: 1, maxBytes: 5120 });
                await db.closeScanCursor(next.cursor!);
                const reverse = [];
                for await (const row of db.scanIter(
                    store,
                    { ...range, reverse: true, limit: 3 },
                    { maxRows: 2, maxBytes: 5120 }
                )) {
                    reverse.push(window.moyodb.utf8Decode(row.key));
                }
                for await (const _row of db.scanIter(store, {}, { maxRows: 1 })) break;
                let oversized = '';
                try {
                    await db.scanPage(store, {}, { maxBytes: 100 });
                } catch (error) {
                    oversized = (error as Error).name;
                }
                observed.push({
                    store,
                    firstRows: first.rows.length,
                    firstBytes: first.bytes,
                    nextValue: next.rows[0].value[0],
                    reverse,
                    oversized,
                    active: (await db.stats()).active_txns
                });
            }
            return observed;
        } finally {
            await db.destroy();
        }
    }, uniqueDbName('primary-cursor'));
    expect(result).toEqual(
        ['raw', 'compressed'].map((store) => ({
            store,
            firstRows: 1,
            firstBytes: 4111,
            nextValue: 5,
            reverse: ['010', '009', '008'],
            oversized: 'ValueTooLargeError',
            active: 0
        }))
    );
});

test('default primary page accepts an incompressible 8 MiB Snappy value with a maximum key', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv', { compression: 'snappy' });
            const value = new Uint8Array(8 * 1024 * 1024);
            let seed = 0x12345678;
            for (let index = 0; index < value.length; index += 1) {
                seed ^= seed << 13;
                seed ^= seed >>> 17;
                seed ^= seed << 5;
                value[index] = seed;
            }
            await db.put('kv', new Uint8Array(1024).fill(42), value);
            const page = await db.scanPage('kv');
            if (page.cursor !== undefined) await db.closeScanCursor(page.cursor);
            return {
                count: page.rows.length,
                bytes: page.bytes,
                length: page.rows[0].value.length,
                first: page.rows[0].value[0] === value[0],
                last: page.rows[0].value.at(-1) === value.at(-1),
                active: (await db.stats()).active_txns
            };
        } finally {
            await db.destroy();
        }
    }, uniqueDbName('primary-cursor-largest'));
    expect(result).toEqual({
        count: 1,
        bytes: 8 * 1024 * 1024 + 1024 + 12,
        length: 8 * 1024 * 1024,
        first: true,
        last: true,
        active: 0
    });
});
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
        },
        { name: dbName, size }
    );
    expect(roundtrip.len).toBe(size);
    expect(roundtrip.first).toBe(0);
    expect(roundtrip.last).toBe((size - 1) % 251);
});
