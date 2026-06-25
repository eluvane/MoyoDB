import { test, expect } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';

test('debug_worker_crash_invalidates_open_handle', async ({ page }) => {
    const dbName = uniqueDbName('worker-crash-handle');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        await db.createStore('kv');
        const crashed = window.moyodb.unsafeDebugCrashWorker(name);
        try {
            await db.listStores();
            return { crashed, errorName: 'NO_ERROR' };
        } catch (error) {
            return { crashed, errorName: (error as Error).name };
        }
    }, dbName);
    expect(result).toEqual({ crashed: true, errorName: 'DatabaseClosedError' });
});

test('debug_worker_crash_closes_active_transaction_and_reopens', async ({ page }) => {
    const dbName = uniqueDbName('worker-crash-tx');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        await db.createStore('kv');
        const tx = await db.begin('readwrite');
        await tx.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        const crashed = window.moyodb.unsafeDebugCrashWorker(name);
        let txErrorName = 'NO_ERROR';
        try {
            await tx.commit();
        } catch (error) {
            txErrorName = (error as Error).name;
        }

        const reopened = await window.moyodb.openDB(name, { ownerWaitMs: 2000, requestPersistence: false });
        try {
            const value = await reopened.get('kv', window.moyodb.utf8Encode('a'));
            return { crashed, txErrorName, value: value ? window.moyodb.utf8Decode(value) : null };
        } finally {
            await reopened.close();
        }
    }, dbName);
    expect(result).toEqual({ crashed: true, txErrorName: 'TransactionClosedError', value: null });
});

test('worker_lifecycle_close_reopen_smoke', async ({ page }) => {
    const dbName = uniqueDbName('worker-lifecycle');
    await prepareMoyoDbPage(page);
    const value = await page.evaluate(async (name) => {
        const first = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await first.createStore('kv');
            await first.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        } finally {
            await first.close();
        }
        const second = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            const bytes = await second.get('kv', window.moyodb.utf8Encode('a'));
            return bytes ? window.moyodb.utf8Decode(bytes) : null;
        } finally {
            await second.close();
        }
    }, dbName);
    expect(value).toBe('1');
});
