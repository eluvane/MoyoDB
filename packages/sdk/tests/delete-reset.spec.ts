import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('db.destroy deletes an open database and closes open transactions', async ({ page }) => {
    const dbName = uniqueDbName('destroy-open');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        await db.createStore('kv');
        await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        const tx = await db.begin('readwrite');
        await tx.put('kv', window.moyodb.utf8Encode('b'), window.moyodb.utf8Encode('2'));
        await db.destroy();
        let dbError = 'NO_ERROR';
        try {
            await db.stats();
        } catch (error) {
            dbError = (error as Error).name;
        }
        let txError = 'NO_ERROR';
        try {
            await tx.get('kv', window.moyodb.utf8Encode('a'));
        } catch (error) {
            txError = (error as Error).name;
        }
        const reopened = await window.moyodb.openDB(name);
        try {
            const stats = await reopened.stats();
            return {
                dbError,
                txError,
                storeCount: stats.store_count
            };
        } finally {
            await reopened.close();
        }
    }, dbName);
    expect(result.dbError).toBe('DatabaseClosedError');
    expect(result.txError).toBe('TransactionClosedError');
    expect(result.storeCount).toBe(0);
});
test('deleteDB removes a database by name without an open handle', async ({ page }) => {
    const dbName = uniqueDbName('delete-by-name');
    await prepareMoyoDbPage(page);
    const storeCount = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        } finally {
            await db.close();
        }
        await window.moyodb.deleteDB(name);
        const reopened = await window.moyodb.openDB(name);
        try {
            const stats = await reopened.stats();
            return stats.store_count;
        } finally {
            await reopened.close();
        }
    }, dbName);
    expect(storeCount).toBe(0);
});
test('db.reset clears data and preserves stores', async ({ page }) => {
    const dbName = uniqueDbName('reset-preserves-stores');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('users');
            await db.createStore('notes');
            await db.put('users', window.moyodb.utf8Encode('alice'), window.moyodb.utf8Encode('1'));
            await db.put('notes', window.moyodb.utf8Encode('n1'), window.moyodb.utf8Encode('draft'));
            const tx = await db.begin('readwrite');
            await tx.put('users', window.moyodb.utf8Encode('bob'), window.moyodb.utf8Encode('2'));
            await db.reset();
            let txError = 'NO_ERROR';
            try {
                await tx.get('users', window.moyodb.utf8Encode('alice'));
            } catch (error) {
                txError = (error as Error).name;
            }
            const stats = await db.stats();
            const userValue = await db.get('users', window.moyodb.utf8Encode('alice'));
            const noteRows = await db.scan('notes');
            await db.put('users', window.moyodb.utf8Encode('fresh'), window.moyodb.utf8Encode('ok'));
            const freshValue = await db.get('users', window.moyodb.utf8Encode('fresh'));
            return {
                txError,
                storeCount: stats.store_count,
                userValue: userValue ? window.moyodb.utf8Decode(userValue) : null,
                noteRows: noteRows.length,
                freshValue: freshValue ? window.moyodb.utf8Decode(freshValue) : null
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.txError).toBe('TransactionClosedError');
    expect(result.storeCount).toBe(2);
    expect(result.userValue).toBeNull();
    expect(result.noteRows).toBe(0);
    expect(result.freshValue).toBe('ok');
});
test('reopening after delete creates a clean database', async ({ page }) => {
    const dbName = uniqueDbName('reopen-after-delete');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        } finally {
            await db.close();
        }
        await window.moyodb.deleteDB(name);
        const reopened = await window.moyodb.openDB(name);
        try {
            await reopened.createStore('kv');
            await reopened.put('kv', window.moyodb.utf8Encode('fresh'), window.moyodb.utf8Encode('value'));
            const value = await reopened.get('kv', window.moyodb.utf8Encode('fresh'));
            return value ? window.moyodb.utf8Decode(value) : null;
        } finally {
            await reopened.close();
        }
    }, dbName);
    expect(result).toBe('value');
});
