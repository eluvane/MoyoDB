import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('exportSnapshot/importSnapshot roundtrip restores database state', async ({ page }) => {
    const source = uniqueDbName('snapshot-source');
    const target = uniqueDbName('snapshot-target');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(
        async ({ sourceName, targetName }) => {
            const sourceDb = await window.moyodb.openDB(sourceName);
            let snapshot: Uint8Array;
            try {
                await sourceDb.createStore('alpha');
                await sourceDb.createStore('empty');
                await sourceDb.put('alpha', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
                await sourceDb.put('alpha', window.moyodb.utf8Encode('b'), window.moyodb.utf8Encode('2'));
                snapshot = await sourceDb.exportSnapshot();
            } finally {
                await sourceDb.close();
            }
            const targetDb = await window.moyodb.openDB(targetName);
            try {
                await targetDb.createStore('junk');
                await targetDb.put('junk', window.moyodb.utf8Encode('stale'), window.moyodb.utf8Encode('value'));
                await targetDb.importSnapshot(snapshot);
                const alphaRows = await targetDb.scan('alpha');
                const emptyRows = await targetDb.scan('empty');
                let junkErrorName: string | null = null;
                try {
                    await targetDb.get('junk', window.moyodb.utf8Encode('stale'));
                } catch (error) {
                    junkErrorName = error instanceof Error ? error.name : String(error);
                }
                return {
                    alphaRows: alphaRows.map((row) => ({
                        key: window.moyodb.utf8Decode(row.key),
                        value: window.moyodb.utf8Decode(row.value)
                    })),
                    emptyCount: emptyRows.length,
                    junkErrorName
                };
            } finally {
                await targetDb.close();
            }
        },
        { sourceName: source, targetName: target }
    );
    expect(result.alphaRows).toEqual([
        { key: 'a', value: '1' },
        { key: 'b', value: '2' }
    ]);
    expect(result.emptyCount).toBe(0);
    expect(result.junkErrorName).toBe('StoreNotFoundError');
});
test('importSnapshot rejects checksum mismatch', async ({ page }) => {
    const name = uniqueDbName('snapshot-checksum');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (dbName) => {
        const sourceDb = await window.moyodb.openDB(`${dbName}-source`);
        let snapshot: Uint8Array;
        try {
            await sourceDb.createStore('alpha');
            await sourceDb.put('alpha', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
            snapshot = await sourceDb.exportSnapshot();
        } finally {
            await sourceDb.close();
        }
        snapshot[32] ^= 0x5a;
        const targetDb = await window.moyodb.openDB(`${dbName}-target`);
        try {
            await targetDb.createStore('keep');
            await targetDb.put('keep', window.moyodb.utf8Encode('k'), window.moyodb.utf8Encode('v'));
            let errorName: string | null = null;
            let errorMessage: string | null = null;
            try {
                await targetDb.importSnapshot(snapshot);
            } catch (error) {
                errorName = error instanceof Error ? error.name : String(error);
                errorMessage = error instanceof Error ? error.message : String(error);
            }
            const kept = await targetDb.get('keep', window.moyodb.utf8Encode('k'));
            return {
                errorName,
                errorMessage,
                kept: kept ? window.moyodb.utf8Decode(kept) : null
            };
        } finally {
            await targetDb.close();
        }
    }, name);
    expect(result.errorName).toBe('CorruptionError');
    expect(result.errorMessage).toContain('checksum');
    expect(result.kept).toBe('v');
});
