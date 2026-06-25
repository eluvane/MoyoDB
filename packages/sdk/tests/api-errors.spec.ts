import { test, expect } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('db.close rolls back handle-local open transactions', async ({ page }) => {
    const dbName = uniqueDbName('close-rolls-back');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        await db.createStore('kv');
        const tx = await db.begin('readwrite');
        await tx.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        await db.close();
        let txError = 'NO_ERROR';
        try {
            await tx.get('kv', window.moyodb.utf8Encode('a'));
        } catch (error) {
            txError = (error as Error).name;
        }
        const reopened = await window.moyodb.openDB(name);
        try {
            const value = await reopened.get('kv', window.moyodb.utf8Encode('a'));
            return {
                txError,
                persisted: value ? window.moyodb.utf8Decode(value) : null
            };
        } finally {
            await reopened.close();
        }
    }, dbName);
    expect(result.txError).toBe('TransactionClosedError');
    expect(result.persisted).toBeNull();
});
test('invalid scan bounds surface InvalidRangeError', async ({ page }) => {
    const dbName = uniqueDbName('invalid-range');
    await prepareMoyoDbPage(page);
    const errorName = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name);
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
            await db.scan('kv', {
                gt: window.moyodb.utf8Encode('a'),
                gte: window.moyodb.utf8Encode('a')
            });
            return 'NO_ERROR';
        } catch (error) {
            return (error as Error).name;
        } finally {
            await db.close();
        }
    }, dbName);
    expect(errorName).toBe('InvalidRangeError');
});
test('openDB rejects invalid option shapes before spawning a worker', async ({ page }) => {
    const dbName = uniqueDbName('invalid-open-options');
    await prepareMoyoDbPage(page);
    const errorName = await page.evaluate(async (name) => {
        try {
            await window.moyodb.openDB(name, { ownerWaitMs: -1 } as any);
            return 'NO_ERROR';
        } catch (error) {
            return (error as Error).name;
        }
    }, dbName);
    expect(errorName).toBe('InvalidOpenOptionsError');
});
test('cachePages=0 is normalized consistently to one page', async ({ page }) => {
    const dbName = uniqueDbName('cache-pages-zero');
    await prepareMoyoDbPage(page);
    const stats = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { cachePages: 0 });
        try {
            return await db.stats();
        } finally {
            await db.close();
        }
    }, dbName);
    expect(stats.cache_pages).toBe(1);
});
test('reopening same db with incompatible cachePages is rejected in-process', async ({ page }) => {
    const dbName = uniqueDbName('cache-pages');
    await prepareMoyoDbPage(page);
    const errorName = await page.evaluate(async (name) => {
        const first = await window.moyodb.openDB(name, { cachePages: 64 });
        try {
            const second = await window.moyodb.openDB(name, { cachePages: 128 });
            await second.close();
            return 'NO_ERROR';
        } catch (error) {
            return (error as Error).name;
        } finally {
            await first.close();
        }
    }, dbName);
    expect(errorName).toBe('InvalidOpenOptionsError');
});
test('openDB and deleteDB validate database names before touching worker state', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async () => {
        const outcomes: Array<{
            name: string;
            message: string;
        }> = [];
        try {
            await window.moyodb.openDB('' as any);
            outcomes.push({ name: 'NO_ERROR', message: '' });
        } catch (error) {
            outcomes.push({
                name: error instanceof Error ? error.name : String(error),
                message: error instanceof Error ? error.message : String(error)
            });
        }
        try {
            await window.moyodb.deleteDB({ bad: true } as any);
            outcomes.push({ name: 'NO_ERROR', message: '' });
        } catch (error) {
            outcomes.push({
                name: error instanceof Error ? error.name : String(error),
                message: error instanceof Error ? error.message : String(error)
            });
        }
        return outcomes;
    });
    expect(result).toEqual([
        {
            name: 'TypeError',
            message: 'openDB() database name must be a non-empty string'
        },
        {
            name: 'TypeError',
            message: 'deleteDB() database name must be a non-empty string'
        }
    ]);
});
test('openDB rejects null option bags with InvalidOpenOptionsError', async ({ page }) => {
    const dbName = uniqueDbName('invalid-open-options-null');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        try {
            await window.moyodb.openDB(name, null as any);
            return { name: 'NO_ERROR', message: '' };
        } catch (error) {
            return {
                name: error instanceof Error ? error.name : String(error),
                message: error instanceof Error ? error.message : String(error)
            };
        }
    }, dbName);
    expect(result).toEqual({
        name: 'InvalidOpenOptionsError',
        message: 'open options must be an object'
    });
});
