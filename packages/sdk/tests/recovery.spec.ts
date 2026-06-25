import { test, expect } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
for (const failpoint of ['after_wal_flush', 'after_main_flush', 'before_superblock_flush'] as const) {
    test(`recovery handles failpoint ${failpoint}`, async ({ page }) => {
        const dbName = uniqueDbName(`recovery-${failpoint}`);
        await prepareMoyoDbPage(page);
        await page.evaluate(async (name) => {
            const db = await window.moyodb.openDB(name);
            try {
                await db.createStore('kv');
                await db.put('kv', window.moyodb.utf8Encode('base'), window.moyodb.utf8Encode('ok'));
            } finally {
                await db.close();
            }
        }, dbName);
        const failureName = await page.evaluate(
            async ({ name, failpoint }) => {
                const db = await window.moyodb.openDB(name);
                try {
                    await db.setFailpoint(failpoint);
                    await db.put('kv', window.moyodb.utf8Encode('after'), window.moyodb.utf8Encode('crashy'));
                    return 'NO_ERROR';
                } catch (error) {
                    return (error as Error).name;
                } finally {
                    await db.close();
                }
            },
            { name: dbName, failpoint }
        );
        expect(failureName).toBe('InjectedFailureError');
        const recovered = await page.evaluate(async (name) => {
            const db = await window.moyodb.openDB(name);
            try {
                const base = await db.get('kv', window.moyodb.utf8Encode('base'));
                const after = await db.get('kv', window.moyodb.utf8Encode('after'));
                return {
                    base: base ? window.moyodb.utf8Decode(base) : null,
                    after: after ? window.moyodb.utf8Decode(after) : null
                };
            } finally {
                await db.close();
            }
        }, dbName);
        expect(recovered.base).toBe('ok');
        if (
            failpoint === 'before_superblock_flush' ||
            failpoint === 'after_main_flush' ||
            failpoint === 'after_wal_flush'
        ) {
            expect(['crashy', null]).toContain(recovered.after);
        }
    });
}
