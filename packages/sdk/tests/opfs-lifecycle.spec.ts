import { test, expect } from '@playwright/test';
import { persistentContextTest, prepareMoyoDbPage, requireMoyoDbCapabilities, uniqueDbName } from './support';

test('opfs_reopen_after_reload', async ({ page }) => {
    const dbName = uniqueDbName('opfs-reload');
    await prepareMoyoDbPage(page);
    await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('reload-key'), window.moyodb.utf8Encode('reload-value'));
        } finally {
            await db.close();
        }
    }, dbName);
    await page.reload();
    await requireMoyoDbCapabilities(page);
    const value = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            const bytes = await db.get('kv', window.moyodb.utf8Encode('reload-key'));
            return bytes ? window.moyodb.utf8Decode(bytes) : null;
        } finally {
            await db.close();
        }
    }, dbName);
    expect(value).toBe('reload-value');
});

test('opfs_reopen_after_new_page_same_context', async ({ context, page }) => {
    const dbName = uniqueDbName('opfs-new-page');
    await prepareMoyoDbPage(page);
    await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('page-key'), window.moyodb.utf8Encode('page-value'));
        } finally {
            await db.close();
        }
    }, dbName);

    const page2 = await context.newPage();
    await page2.goto('/');
    await requireMoyoDbCapabilities(page2);
    const value = await page2.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            const bytes = await db.get('kv', window.moyodb.utf8Encode('page-key'));
            return bytes ? window.moyodb.utf8Decode(bytes) : null;
        } finally {
            await db.close();
        }
    }, dbName);
    await page2.close();
    expect(value).toBe('page-value');
});

persistentContextTest('opfs_reopen_after_new_context', async ({ persistentContextFactory }) => {
    const dbName = uniqueDbName('opfs-new-context');
    const context1 = await persistentContextFactory.launch();
    const page1 = context1.pages()[0] ?? (await context1.newPage());
    await prepareMoyoDbPage(page1);
    await page1.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('context-key'), window.moyodb.utf8Encode('context-value'));
        } finally {
            await db.close();
        }
    }, dbName);
    await context1.close();

    const context2 = await persistentContextFactory.launch();
    const page2 = context2.pages()[0] ?? (await context2.newPage());
    await prepareMoyoDbPage(page2);
    const value = await page2.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            const bytes = await db.get('kv', window.moyodb.utf8Encode('context-key'));
            return bytes ? window.moyodb.utf8Decode(bytes) : null;
        } finally {
            await db.close();
        }
    }, dbName);
    await context2.close();
    expect(value).toBe('context-value');
});

test('storageInfo_usage_changes_after_bulk_insert', async ({ page }) => {
    const dbName = uniqueDbName('opfs-usage');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            const before = await db.storageInfo();
            const value = new Uint8Array(512);
            for (let i = 0; i < value.length; i += 1) value[i] = i & 0xff;
            const tx = await db.begin('readwrite');
            try {
                const entries: Array<[Uint8Array, Uint8Array]> = [];
                for (let i = 0; i < 1000; i += 1) {
                    entries.push([window.moyodb.utf8Encode(`k-${i.toString().padStart(4, '0')}`), value]);
                }
                await tx.putMany('kv', entries);
                await tx.commit();
            } catch (error) {
                await tx.rollback().catch(() => undefined);
                throw error;
            }
            const after = await db.storageInfo();
            return { before, after };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.after.dbSize).toBeGreaterThan(result.before.dbSize);
    expect(result.after.originQuota).toBeGreaterThanOrEqual(result.after.originUsage);
});

test('requestPersistence_returns_boolean', async ({ page }) => {
    const dbName = uniqueDbName('opfs-persist-bool');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            return await db.requestPersistence();
        } finally {
            await db.close();
        }
    }, dbName);
    expect(typeof result).toBe('boolean');
});

test('deleteDB_removes_opfs_files', async ({ page }) => {
    const dbName = uniqueDbName('opfs-delete');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        function encodedDbName(value: string): string {
            return Array.from(new TextEncoder().encode(value), (byte) => byte.toString(16).padStart(2, '0')).join('');
        }
        async function hasDbDirectory(): Promise<boolean> {
            const root = await navigator.storage.getDirectory();
            try {
                const stackdb = await root.getDirectoryHandle('stackdb');
                await stackdb.getDirectoryHandle(encodedDbName(name));
                return true;
            } catch {
                return false;
            }
        }

        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        } finally {
            await db.close();
        }
        const existsBeforeDelete = await hasDbDirectory();
        await window.moyodb.deleteDB(name);
        const existsAfterDelete = await hasDbDirectory();
        return { existsBeforeDelete, existsAfterDelete };
    }, dbName);
    expect(result.existsBeforeDelete).toBe(true);
    expect(result.existsAfterDelete).toBe(false);
});

test('second_tab_owner_rejected', async ({ browser }) => {
    const dbName = uniqueDbName('opfs-second-tab');
    const context = await browser.newContext();
    const page1 = await context.newPage();
    const page2 = await context.newPage();
    await page1.goto('/');
    await page2.goto('/');
    await requireMoyoDbCapabilities(page1);
    await page1.evaluate(async (name) => {
        (window as any).__heldDb = await window.moyodb.openDB(name, { requestPersistence: false });
    }, dbName);
    const errorName = await page2.evaluate(async (name) => {
        try {
            const db = await window.moyodb.openDB(name, { ownerWaitMs: 0, requestPersistence: false });
            await db.close();
            return 'NO_ERROR';
        } catch (error) {
            return (error as Error).name;
        }
    }, dbName);
    await page1.evaluate(async () => {
        const db = (window as any).__heldDb;
        await db.close();
    });
    await context.close();
    expect(errorName).toBe('DatabaseBusyError');
});

test('unsupported_opfs_path_returns_clear_error', async ({ page }) => {
    await page.goto('/');
    const errorName = await page.evaluate(async () => {
        const storage = navigator.storage as (StorageManager & { getDirectory?: unknown }) | undefined;
        if (!storage) {
            try {
                const db = await window.moyodb.openDB('unsupported-opfs-path');
                await db.close();
                return 'NO_ERROR';
            } catch (error) {
                return (error as Error).name;
            }
        }
        const original = storage.getDirectory;
        try {
            Object.defineProperty(storage, 'getDirectory', { configurable: true, value: undefined });
            try {
                const db = await window.moyodb.openDB('unsupported-opfs-path');
                await db.close();
                return 'NO_ERROR';
            } catch (error) {
                return (error as Error).name;
            }
        } finally {
            Object.defineProperty(storage, 'getDirectory', { configurable: true, value: original });
        }
    });
    expect(errorName).toBe('UnsupportedPlatformError');
});

test('safari_firefox_chromium_compatibility_smoke', async ({ page, browserName }) => {
    const dbName = uniqueDbName(`compat-${browserName}`);
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('browser'), window.moyodb.utf8Encode(navigator.userAgent));
            const value = await db.get('kv', window.moyodb.utf8Encode('browser'));
            const info = await db.storageInfo();
            return { value: value ? window.moyodb.utf8Decode(value) : null, quota: info.originQuota };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.value).toContain(browserName === 'webkit' ? 'AppleWebKit' : '');
    expect(result.quota).toBeGreaterThanOrEqual(0);
});
