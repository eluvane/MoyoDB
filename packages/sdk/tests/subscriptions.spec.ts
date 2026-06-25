import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, requireMoyoDbCapabilities, uniqueDbName } from './support';
test('db.subscribe(callback) emits one callback per touched store with shared txnId', async ({ page }) => {
    const dbName = uniqueDbName('subscribe-all');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('users');
            await db.createStore('posts');
            const received: Array<{
                store: string;
                changes: Array<{
                    key: string;
                    kind: 'put' | 'delete';
                }>;
                txnId: number;
            }> = [];
            const done = new Promise<void>((resolve) => {
                const unsub = db.subscribe((store, changes, txnId) => {
                    received.push({
                        store,
                        changes: changes.map((change) => ({
                            key: window.moyodb.utf8Decode(change.key),
                            kind: change.kind
                        })),
                        txnId
                    });
                    if (received.length === 2) {
                        unsub();
                        resolve();
                    }
                });
            });
            const tx = await db.begin('readwrite');
            await tx.put('users', window.moyodb.utf8Encode('user:1'), window.moyodb.utf8Encode('alice'));
            await tx.put('posts', window.moyodb.utf8Encode('post:1'), window.moyodb.utf8Encode('hello'));
            await tx.commit();
            await done;
            return received.sort((left, right) => left.store.localeCompare(right.store));
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toHaveLength(2);
    expect(result[0].store).toBe('posts');
    expect(result[0].changes).toEqual([{ key: 'post:1', kind: 'put' }]);
    expect(result[1].store).toBe('users');
    expect(result[1].changes).toEqual([{ key: 'user:1', kind: 'put' }]);
    expect(result[0].txnId).toBeGreaterThan(0);
    expect(result[1].txnId).toBe(result[0].txnId);
});
test('db.subscribe(storeName, callback) filters by store and unsubscribe stops delivery', async ({ page }) => {
    const dbName = uniqueDbName('subscribe-store');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('users');
            await db.createStore('logs');
            const received: Array<{
                store: string;
                changes: Array<{
                    key: string;
                    kind: 'put' | 'delete';
                }>;
                txnId: number;
            }> = [];
            const firstDelivery = new Promise<void>((resolve) => {
                const unsub = db.subscribe('users', (store, changes, txnId) => {
                    received.push({
                        store,
                        changes: changes.map((change) => ({
                            key: window.moyodb.utf8Decode(change.key),
                            kind: change.kind
                        })),
                        txnId
                    });
                    unsub();
                    resolve();
                });
            });
            await db.put('users', window.moyodb.utf8Encode('user:1'), window.moyodb.utf8Encode('alice'));
            await firstDelivery;
            await db.put('logs', window.moyodb.utf8Encode('log:1'), window.moyodb.utf8Encode('ignored'));
            await db.put('users', window.moyodb.utf8Encode('user:2'), window.moyodb.utf8Encode('ignored-after-unsub'));
            await new Promise((resolve) => setTimeout(resolve, 50));
            return received;
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toEqual([
        {
            store: 'users',
            changes: [{ key: 'user:1', kind: 'put' }],
            txnId: expect.any(Number)
        }
    ]);
    expect(result[0].txnId).toBeGreaterThan(0);
});
test('db.subscribe(storeName, keyPrefix, callback) filters matching keys inside a commit', async ({ page }) => {
    const dbName = uniqueDbName('subscribe-prefix');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('users');
            const event = new Promise<{
                store: string;
                changes: Array<{
                    key: string;
                    kind: 'put' | 'delete';
                }>;
                txnId: number;
            }>((resolve) => {
                const unsub = db.subscribe('users', window.moyodb.utf8Encode('user:'), (store, changes, txnId) => {
                    unsub();
                    resolve({
                        store,
                        changes: changes.map((change) => ({
                            key: window.moyodb.utf8Decode(change.key),
                            kind: change.kind
                        })),
                        txnId
                    });
                });
            });
            const tx = await db.begin('readwrite');
            await tx.put('users', window.moyodb.utf8Encode('user:1'), window.moyodb.utf8Encode('alice'));
            await tx.put('users', window.moyodb.utf8Encode('meta:1'), window.moyodb.utf8Encode('skip'));
            await tx.put('users', window.moyodb.utf8Encode('user:2'), window.moyodb.utf8Encode('bob'));
            await tx.commit();
            return await event;
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.store).toBe('users');
    expect(result.changes).toEqual([
        { key: 'user:1', kind: 'put' },
        { key: 'user:2', kind: 'put' }
    ]);
    expect(result.txnId).toBeGreaterThan(0);
});
test('BroadcastChannel publishes detailed commit payloads across tabs', async ({ browser }) => {
    const dbName = uniqueDbName('subscribe-cross-tab');
    const context = await browser.newContext();
    const writer = await context.newPage();
    const observer = await context.newPage();
    await writer.goto('/');
    await observer.goto('/');
    await requireMoyoDbCapabilities(writer);
    await writer.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('users');
        } finally {
            await db.close();
        }
    }, dbName);
    const eventPromise = observer.evaluate((name) => {
        return new Promise<{
            txid: number;
            stores: Array<{
                store: string;
                changes: Array<{
                    key: string;
                    kind: 'put' | 'delete';
                }>;
            }>;
        }>((resolve) => {
            const channel = new BroadcastChannel(`db:${name}:events`);
            channel.onmessage = (event) => {
                const payload = event.data;
                if (
                    payload?.type !== 'commit_applied' ||
                    typeof payload?.txid !== 'number' ||
                    !Array.isArray(payload?.stores)
                ) {
                    return;
                }
                const stores: Array<{
                    store: string;
                    changes: Array<{
                        key: string;
                        kind: 'put' | 'delete';
                    }>;
                }> = payload.stores.map(
                    (storeEvent: {
                        store: string;
                        changes: Array<{
                            key: Uint8Array;
                            kind: 'put' | 'delete';
                        }>;
                    }) => ({
                        store: storeEvent.store,
                        changes: storeEvent.changes.map((change) => ({
                            key: window.moyodb.utf8Decode(change.key),
                            kind: change.kind
                        }))
                    })
                );
                const users = stores.find((entry) => entry.store === 'users');
                if (!users || users.changes.length === 0) {
                    return;
                }
                resolve({ txid: payload.txid, stores });
                channel.close();
            };
        });
    }, dbName);
    await writer.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            const tx = await db.begin('readwrite');
            await tx.put('users', window.moyodb.utf8Encode('user:1'), window.moyodb.utf8Encode('alice'));
            await tx.put('users', window.moyodb.utf8Encode('user:2'), window.moyodb.utf8Encode('bob'));
            await tx.commit();
        } finally {
            await db.close();
        }
    }, dbName);
    const event = await eventPromise;
    expect(event.txid).toBeGreaterThan(0);
    expect(event.stores).toEqual([
        {
            store: 'users',
            changes: [
                { key: 'user:1', kind: 'put' },
                { key: 'user:2', kind: 'put' }
            ]
        }
    ]);
});
test('db.subscribe and db.watch reject reserved internal store names', async ({ page }) => {
    const dbName = uniqueDbName('subscribe-reserved-store');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            const outcomes: Array<{
                name: string;
                message: string;
            }> = [];
            try {
                db.subscribe('__browserdb:indexes', () => undefined);
                outcomes.push({ name: 'NO_ERROR', message: '' });
            } catch (error) {
                outcomes.push({
                    name: error instanceof Error ? error.name : String(error),
                    message: error instanceof Error ? error.message : String(error)
                });
            }
            try {
                db.watch('__browserdb:indexes', () => undefined);
                outcomes.push({ name: 'NO_ERROR', message: '' });
            } catch (error) {
                outcomes.push({
                    name: error instanceof Error ? error.name : String(error),
                    message: error instanceof Error ? error.message : String(error)
                });
            }
            return outcomes;
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toEqual([
        {
            name: 'ReservedStoreNameError',
            message: 'reserved store name: __browserdb:indexes'
        },
        {
            name: 'ReservedStoreNameError',
            message: 'reserved store name: __browserdb:indexes'
        }
    ]);
});
