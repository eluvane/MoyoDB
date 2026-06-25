import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('db.compact rebuilds live state, reclaims space, and invalidates open transactions', async ({ page }) => {
    test.setTimeout(120_000);
    const dbName = uniqueDbName('compact');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        const payloadFor = (label: string) => `${label}:${'x'.repeat(4096)}`;
        try {
            await db.createStore('docs');
            for (let index = 0; index < 96; index += 1) {
                await db.put(
                    'docs',
                    encode(`k${index.toString().padStart(3, '0')}`),
                    encode(payloadFor(`v1-${index}`))
                );
            }
            for (let index = 0; index < 96; index += 1) {
                await db.put(
                    'docs',
                    encode(`k${index.toString().padStart(3, '0')}`),
                    encode(payloadFor(`v2-${index}`))
                );
            }
            for (let index = 0; index < 96; index += 3) {
                await db.delete('docs', encode(`k${index.toString().padStart(3, '0')}`));
            }
            const infoBefore = await db.storageInfo();
            const tx = await db.begin('readwrite');
            await tx.put('docs', encode('tx-only'), encode(payloadFor('uncommitted')));
            const compacted = await db.compact();
            const infoAfter = await db.storageInfo();
            const survivingRows = await db.scan('docs');
            const liveValue = await db.get('docs', encode('k001'));
            const deletedValue = await db.get('docs', encode('k000'));
            const uncommittedValue = await db.get('docs', encode('tx-only'));
            let txError = 'NO_ERROR';
            try {
                await tx.get('docs', encode('k001'));
            } catch (error) {
                txError = error instanceof Error ? error.name : String(error);
            }
            return {
                infoBefore,
                infoAfter,
                compacted,
                rowCount: survivingRows.length,
                liveValue: liveValue ? decode(liveValue).slice(0, 8) : null,
                deletedValue: deletedValue ? decode(deletedValue) : null,
                uncommittedValue: uncommittedValue ? decode(uncommittedValue) : null,
                txError
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.compacted.sizeBefore).toBe(result.infoBefore.dbSize);
    expect(result.compacted.sizeAfter).toBe(result.infoAfter.dbSize);
    expect(result.compacted.reclaimedBytes).toBe(Math.max(0, result.compacted.sizeBefore - result.compacted.sizeAfter));
    expect(result.compacted.durationMs).toBeGreaterThanOrEqual(0);
    expect(result.compacted.sizeAfter).toBeLessThan(result.compacted.sizeBefore);
    expect(result.compacted.reclaimedBytes).toBeGreaterThan(0);
    expect(result.rowCount).toBe(64);
    expect(result.liveValue).toBe('v2-1:xxx');
    expect(result.deletedValue).toBeNull();
    expect(result.uncommittedValue).toBeNull();
    expect(result.txError).toBe('TransactionClosedError');
});
test('db.rebuild swaps in a fresh generation and survives close/reopen', async ({ page }) => {
    const dbName = uniqueDbName('rebuild');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        const payloadFor = (label: string) => `${label}:${'y'.repeat(3072)}`;
        try {
            await db.createStore('docs');
            await db.createStore('meta');
            for (let index = 0; index < 72; index += 1) {
                await db.put(
                    'docs',
                    encode(`k${index.toString().padStart(3, '0')}`),
                    encode(payloadFor(`v1-${index}`))
                );
            }
            for (let index = 0; index < 72; index += 1) {
                await db.put(
                    'docs',
                    encode(`k${index.toString().padStart(3, '0')}`),
                    encode(payloadFor(`v2-${index}`))
                );
            }
            for (let index = 0; index < 72; index += 4) {
                await db.delete('docs', encode(`k${index.toString().padStart(3, '0')}`));
            }
            await db.put('meta', encode('schema'), encode('1'));
            const infoBefore = await db.storageInfo();
            const rebuilt = await db.rebuild();
            const infoAfter = await db.storageInfo();
            await db.close();
            const reopened = await window.moyodb.openDB(name, { requestPersistence: false });
            try {
                const docs = await reopened.scan('docs');
                const meta = await reopened.get('meta', encode('schema'));
                const deleted = await reopened.get('docs', encode('k000'));
                const live = await reopened.get('docs', encode('k001'));
                const stats = await reopened.stats();
                return {
                    infoBefore,
                    infoAfter,
                    rebuilt,
                    rowCount: docs.length,
                    meta: meta ? decode(meta) : null,
                    deleted: deleted ? decode(deleted) : null,
                    liveValue: live ? decode(live).slice(0, 8) : null,
                    storeCount: stats.store_count
                };
            } finally {
                await reopened.close();
            }
        } catch (error) {
            try {
                await db.close();
            } catch {}
            throw error;
        }
    }, dbName);
    expect(result.rebuilt.sizeBefore).toBe(result.infoBefore.dbSize);
    expect(result.rebuilt.sizeAfter).toBe(result.infoAfter.dbSize);
    expect(result.rebuilt.reclaimedBytes).toBe(Math.max(0, result.rebuilt.sizeBefore - result.rebuilt.sizeAfter));
    expect(result.rebuilt.durationMs).toBeGreaterThanOrEqual(0);
    expect(result.rebuilt.sizeAfter).toBeLessThan(result.rebuilt.sizeBefore);
    expect(result.rebuilt.reclaimedBytes).toBeGreaterThan(0);
    expect(result.rowCount).toBe(54);
    expect(result.meta).toBe('1');
    expect(result.deleted).toBeNull();
    expect(result.liveValue).toBe('v2-1:yyy');
    expect(result.storeCount).toBe(2);
});
