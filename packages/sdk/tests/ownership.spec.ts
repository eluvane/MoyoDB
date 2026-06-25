import { test, expect } from '@playwright/test';
import { requireMoyoDbCapabilities, uniqueDbName } from './support';
test('second tab cannot acquire ownership while first owner is open', async ({ browser }) => {
    const dbName = uniqueDbName('ownership');
    const context = await browser.newContext();
    const page1 = await context.newPage();
    const page2 = await context.newPage();
    await page1.goto('/');
    await page2.goto('/');
    await requireMoyoDbCapabilities(page1);
    await page1.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        (window as any).__heldDb = db;
    }, dbName);
    const errorName = await page2.evaluate(async (name) => {
        try {
            const db = await window.moyodb.openDB(name, { ownerWaitMs: 0 });
            await db.close();
            return 'NO_ERROR';
        } catch (error) {
            return (error as Error).name;
        }
    }, dbName);
    expect(errorName).toBe('DatabaseBusyError');
    await page1.evaluate(async () => {
        const db = (window as any).__heldDb;
        await db.close();
    });
    const reopened = await page2.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { ownerWaitMs: 2000 });
        try {
            return (await db.stats()).store_count;
        } finally {
            await db.close();
        }
    }, dbName);
    expect(reopened).toBe(0);
});
test('BroadcastChannel receives commit_applied event', async ({ browser }) => {
    const dbName = uniqueDbName('events');
    const context = await browser.newContext();
    const page1 = await context.newPage();
    const page2 = await context.newPage();
    await page1.goto('/');
    await page2.goto('/');
    await requireMoyoDbCapabilities(page1);
    const eventPromise = page2.evaluate((name) => {
        return new Promise((resolve) => {
            const channel = new BroadcastChannel(`db:${name}:events`);
            channel.onmessage = (event) => {
                if (event.data?.type === 'commit_applied') {
                    resolve(event.data);
                    channel.close();
                }
            };
        });
    }, dbName);
    await page1.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        } finally {
            await db.close();
        }
    }, dbName);
    const event = (await eventPromise) as {
        type: string;
        dbName: string;
        txid: number;
    };
    expect(event.type).toBe('commit_applied');
    expect(event.dbName).toBe(dbName);
    expect(event.txid).toBeGreaterThan(0);
});
