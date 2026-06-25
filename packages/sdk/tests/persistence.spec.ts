import { test, expect } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('open/create/put/get persists across reopen', async ({ page }) => {
    const dbName = uniqueDbName('persistence');
    await prepareMoyoDbPage(page);
    await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        } finally {
            await db.close();
        }
    }, dbName);
    const value = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            const bytes = await db.get('kv', window.moyodb.utf8Encode('a'));
            return bytes ? window.moyodb.utf8Decode(bytes) : null;
        } finally {
            await db.close();
        }
    }, dbName);
    expect(value).toBe('1');
});
