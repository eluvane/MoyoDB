import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('openDB runs migrate on first versioned open and skips same-version reopens', async ({ page }) => {
    const dbName = uniqueDbName('version-init');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const calls: Array<{
            oldVersion: number;
            newVersion: number;
        }> = [];
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        const first = await window.moyodb.openDB(name, {
            version: 1,
            migrate: async ({ db, oldVersion, newVersion }) => {
                calls.push({ oldVersion, newVersion });
                await db.createStore('notes');
                await db.put('notes', encode('note:1'), encode('hello'));
            }
        });
        let firstSnapshot: {
            stores: string[];
            version: number;
            note: string | null;
        };
        try {
            firstSnapshot = {
                stores: await first.listStores(),
                version: await first.getVersion(),
                note: decode((await first.get('notes', encode('note:1')))!)
            };
        } finally {
            await first.close();
        }
        const second = await window.moyodb.openDB(name, {
            version: 1,
            migrate: async ({ oldVersion, newVersion }) => {
                calls.push({ oldVersion, newVersion });
            }
        });
        try {
            return {
                calls,
                firstSnapshot,
                secondSnapshot: {
                    stores: await second.listStores(),
                    version: await second.getVersion(),
                    note: decode((await second.get('notes', encode('note:1')))!)
                }
            };
        } finally {
            await second.close();
        }
    }, dbName);
    expect(result.calls).toEqual([{ oldVersion: 0, newVersion: 1 }]);
    expect(result.firstSnapshot).toEqual({
        stores: ['notes'],
        version: 1,
        note: 'hello'
    });
    expect(result.secondSnapshot).toEqual({
        stores: ['notes'],
        version: 1,
        note: 'hello'
    });
});
test('openDB upgrades atomically and rejects downgrades', async ({ page }) => {
    const dbName = uniqueDbName('version-upgrade');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        const v1 = await window.moyodb.openDB(name, {
            version: 1,
            migrate: async ({ db }) => {
                await db.createStore('users_v1');
                await db.put('users_v1', encode('user:1'), encode('alice'));
            }
        });
        await v1.close();
        const upgradeSteps: Array<{
            oldVersion: number;
            newVersion: number;
        }> = [];
        const v2 = await window.moyodb.openDB(name, {
            version: 2,
            migrate: async ({ oldVersion, newVersion, transaction }) => {
                upgradeSteps.push({ oldVersion, newVersion });
                await transaction.createStore('users');
                const rows = await transaction.scan('users_v1');
                for (const row of rows) {
                    await transaction.put('users', row.key, row.value);
                }
                await transaction.dropStore('users_v1');
            }
        });
        let downgradeError: {
            name: string;
            message: string;
        } | null = null;
        try {
            await window.moyodb.openDB(name, {
                version: 1,
                migrate: async () => {
                    throw new Error('should not run');
                }
            });
        } catch (error) {
            downgradeError = {
                name: error instanceof Error ? error.name : String(error),
                message: error instanceof Error ? error.message : String(error)
            };
        }
        try {
            return {
                upgradeSteps,
                stores: await v2.listStores(),
                version: await v2.getVersion(),
                user: decode((await v2.get('users', encode('user:1')))!),
                downgradeError
            };
        } finally {
            await v2.close();
        }
    }, dbName);
    expect(result.upgradeSteps).toEqual([{ oldVersion: 1, newVersion: 2 }]);
    expect(result.stores).toEqual(['users']);
    expect(result.version).toBe(2);
    expect(result.user).toBe('alice');
    expect(result.downgradeError?.name).toBe('VersionError');
    expect(result.downgradeError?.message).toContain('current schema version is 2');
});
test('failed migrations rollback schema and version changes', async ({ page }) => {
    const dbName = uniqueDbName('version-rollback');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        const base = await window.moyodb.openDB(name, {
            version: 1,
            migrate: async ({ db }) => {
                await db.createStore('stable');
                await db.put('stable', encode('k'), encode('v1'));
            }
        });
        await base.close();
        let openError: {
            name: string;
            message: string;
        } | null = null;
        try {
            await window.moyodb.openDB(name, {
                version: 2,
                migrate: async ({ db }) => {
                    await db.createStore('temp');
                    await db.put('temp', encode('scratch'), encode('value'));
                    throw new Error('boom');
                }
            });
        } catch (error) {
            openError = {
                name: error instanceof Error ? error.name : String(error),
                message: error instanceof Error ? error.message : String(error)
            };
        }
        const reopened = await window.moyodb.openDB(name, {
            version: 1,
            migrate: async () => {
                throw new Error('should not run');
            }
        });
        try {
            return {
                openError,
                stores: await reopened.listStores(),
                version: await reopened.getVersion(),
                stable: decode((await reopened.get('stable', encode('k')))!),
                tempExists: (await reopened.listStores()).includes('temp')
            };
        } finally {
            await reopened.close();
        }
    }, dbName);
    expect(result.openError).toEqual({ name: 'Error', message: 'boom' });
    expect(result.stores).toEqual(['stable']);
    expect(result.version).toBe(1);
    expect(result.stable).toBe('v1');
    expect(result.tempExists).toBe(false);
});
