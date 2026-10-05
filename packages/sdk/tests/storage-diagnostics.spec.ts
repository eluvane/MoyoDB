import { test, expect } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('storageInfo reports db size plus origin-wide diagnostics', async ({ page }) => {
    const dbName = uniqueDbName('storage-info');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
            const stats = await db.stats();
            const info = await db.storageInfo();
            return {
                stats,
                info,
                expectedDbSize: stats.manifest_len + stats.main_len + stats.wal_len
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.info.dbSize).toBe(result.expectedDbSize);
    expect(result.info.dbSize).toBeGreaterThan(0);
    expect(result.info.originUsage).toBeGreaterThanOrEqual(0);
    expect(result.info.originQuota).toBeGreaterThanOrEqual(0);
    expect(result.info.originQuota).toBeGreaterThanOrEqual(result.info.originUsage);
    expect(typeof result.info.persisted).toBe('boolean');
});
test('requestPersistence resolves to a boolean and is reflected when granted', async ({ page }) => {
    const dbName = uniqueDbName('request-persistence');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            const before = await db.storageInfo();
            const granted = await db.requestPersistence();
            const after = await db.storageInfo();
            return {
                beforePersisted: before.persisted,
                granted,
                afterPersisted: after.persisted
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(typeof result.beforePersisted).toBe('boolean');
    expect(typeof result.granted).toBe('boolean');
    expect(typeof result.afterPersisted).toBe('boolean');
    if (result.granted) {
        expect(result.afterPersisted).toBe(true);
    }
});
