import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('db.changesSince returns durable filtered changes and respects limit', async ({ page }) => {
    const dbName = uniqueDbName('changes-feed');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        try {
            await db.createStore('docs');
            await db.createStore('meta');
            await db.put('docs', encode('seed'), encode('v1'));
            await db.put('meta', encode('m'), encode('keep'));
            const seedTxId = (await db.stats()).last_committed_txid;
            await db.put('docs', encode('seed'), encode('v2'));
            await db.put('docs', encode('next'), encode('v3'));
            await db.delete('meta', encode('m'));
            const latestTxId = (await db.stats()).last_committed_txid;
            await db.close();
            const reopened = await window.moyodb.openDB(name, { requestPersistence: false });
            try {
                const filtered = await reopened.changesSince(seedTxId, { stores: ['docs'] });
                const limited = await reopened.changesSince(seedTxId, { limit: 1 });
                return {
                    latestTxId,
                    filteredLatestTxId: filtered.latestTxId,
                    filtered: filtered.changes.map((change) => ({
                        txId: change.txId,
                        store: change.store,
                        key: decode(change.key),
                        kind: change.kind,
                        value: change.value ? decode(change.value) : null
                    })),
                    limitedLatestTxId: limited.latestTxId,
                    limitedCount: limited.changes.length,
                    limitedFirstKey: limited.changes[0] ? decode(limited.changes[0].key) : null,
                    limitedFirstKind: limited.changes[0]?.kind ?? null
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
    expect(result.filteredLatestTxId).toBe(result.latestTxId);
    expect(result.filtered).toHaveLength(2);
    expect(result.filtered[0]).toMatchObject({ store: 'docs', key: 'seed', kind: 'put', value: 'v2' });
    expect(result.filtered[1]).toMatchObject({ store: 'docs', key: 'next', kind: 'put', value: 'v3' });
    expect(result.filtered[0]!.txId).toBeLessThan(result.filtered[1]!.txId);
    expect(result.filtered[1]!.txId).toBeLessThan(result.latestTxId);
    expect(result.limitedLatestTxId).toBe(result.latestTxId);
    expect(result.limitedCount).toBe(1);
    expect(result.limitedFirstKind).toBe('put');
    expect(result.limitedFirstKey).toBe('seed');
});
test('db.changesSince throws explicit error after compaction drops an old cursor', async ({ page }) => {
    const dbName = uniqueDbName('changes-feed-compact');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        const encode = window.moyodb.utf8Encode;
        try {
            await db.createStore('docs');
            await db.put('docs', encode('a'), encode('one'));
            const seedTxId = (await db.stats()).last_committed_txid;
            await db.compact();
            const retained = await db.changesSince(seedTxId);
            let compactedName = 'NO_ERROR';
            let compactedMessage = '';
            try {
                await db.changesSince(seedTxId - 1);
            } catch (error) {
                compactedName = error instanceof Error ? error.name : String(error);
                compactedMessage = error instanceof Error ? error.message : String(error);
            }
            return {
                seedTxId,
                retainedLatestTxId: retained.latestTxId,
                retainedCount: retained.changes.length,
                compactedName,
                compactedMessage
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.retainedLatestTxId).toBeGreaterThan(result.seedTxId);
    expect(result.retainedCount).toBe(0);
    expect(result.compactedName).toBe('ChangeFeedCompactedError');
    expect(result.compactedMessage).toContain('older than retained floor');
});
test('db.changesSince rejects reserved internal store filters', async ({ page }) => {
    const dbName = uniqueDbName('changes-feed-reserved-store');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('docs');
            try {
                await db.changesSince(0, { stores: ['__browserdb:indexes'] as any });
                return { name: 'NO_ERROR', message: '' };
            } catch (error) {
                return {
                    name: error instanceof Error ? error.name : String(error),
                    message: error instanceof Error ? error.message : String(error)
                };
            }
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toEqual({
        name: 'ReservedStoreNameError',
        message: 'reserved store name: __browserdb:indexes'
    });
});
