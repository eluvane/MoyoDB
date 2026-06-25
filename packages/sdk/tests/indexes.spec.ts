import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('managed indexes are hidden from catalog and support exact + range lookups', async ({ page }) => {
    const dbName = uniqueDbName('indexes-basic');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        const decodeJson = window.moyodb.jsonDecode as <T>(bytes: Uint8Array) => T;
        const db = await window.moyodb.openDB(name, {
            version: 1,
            indexes: [
                { store: 'users', name: 'byEmail', keyPath: 'email', unique: true },
                { store: 'users', name: 'byOrgAndAge', keyPath: ['org', 'age'] }
            ],
            migrate: async ({ db }) => {
                await db.createStore('users');
            }
        });
        try {
            const seedTxId = (await db.stats()).last_committed_txid;
            await db.put(
                'users',
                encode('u:2'),
                window.moyodb.jsonEncode({ email: 'bea@example.com', org: 'acme', age: 31, name: 'Bea' })
            );
            await db.put(
                'users',
                encode('u:1'),
                window.moyodb.jsonEncode({ email: 'ada@example.com', org: 'acme', age: 30, name: 'Ada' })
            );
            await db.put(
                'users',
                encode('u:3'),
                window.moyodb.jsonEncode({ email: 'cy@example.com', org: 'beta', age: 29, name: 'Cy' })
            );
            const tx = await db.begin('readonly');
            try {
                const exact = await tx.getByIndex('users', 'byEmail', window.moyodb.indexKey('ada@example.com'));
                const byOrg: Array<{
                    key: string;
                    name: string;
                }> = [];
                for await (const [key, value] of tx.scanByIndex(
                    'users',
                    'byOrgAndAge',
                    window.moyodb.compoundKeyRange('acme')
                )) {
                    byOrg.push({
                        key: decode(key),
                        name: decodeJson<{
                            name: string;
                        }>(value).name
                    });
                }
                const feed = await db.changesSince(seedTxId, { limit: 10 });
                return {
                    listStores: await db.listStores(),
                    stats: await db.stats(),
                    exactName: exact
                        ? decodeJson<{
                              name: string;
                          }>(exact).name
                        : null,
                    byOrg,
                    changeStores: [...new Set(feed.changes.map((change) => change.store))],
                    changeCount: feed.changes.length
                };
            } finally {
                await tx.rollback();
            }
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.listStores).toEqual(['users']);
    expect(result.stats.store_count).toBe(1);
    expect(result.exactName).toBe('Ada');
    expect(result.byOrg).toEqual([
        { key: 'u:1', name: 'Ada' },
        { key: 'u:2', name: 'Bea' }
    ]);
    expect(result.changeStores).toEqual(['users']);
    expect(result.changeCount).toBe(3);
});
test('managed indexes follow createStore, put, clearStore, and dropStore semantics', async ({ page }) => {
    const dbName = uniqueDbName('indexes-store-lifecycle');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const encode = window.moyodb.utf8Encode;
        const decodeJson = window.moyodb.jsonDecode as <T>(bytes: Uint8Array) => T;
        const db = await window.moyodb.openDB(name, {
            version: 1,
            indexes: [{ store: 'users', name: 'byEmail', keyPath: 'email' }],
            migrate: async () => {}
        });
        try {
            await db.createStore('users');
            await db.put(
                'users',
                encode('u:1'),
                window.moyodb.jsonEncode({ email: 'before@example.com', name: 'Before' })
            );
            const tx = await db.begin('readwrite');
            let beforeCommitName: string | null = null;
            let oldLookupBeforeCommit: string | null = null;
            try {
                await tx.put(
                    'users',
                    encode('u:1'),
                    window.moyodb.jsonEncode({ email: 'after@example.com', name: 'After' })
                );
                const beforeCommit = await tx.getByIndex(
                    'users',
                    'byEmail',
                    window.moyodb.indexKey('after@example.com')
                );
                const oldLookup = await tx.getByIndex('users', 'byEmail', window.moyodb.indexKey('before@example.com'));
                beforeCommitName = beforeCommit
                    ? decodeJson<{
                          name: string;
                      }>(beforeCommit).name
                    : null;
                oldLookupBeforeCommit = oldLookup
                    ? decodeJson<{
                          name: string;
                      }>(oldLookup).name
                    : null;
                await tx.commit();
            } catch (error) {
                try {
                    await tx.rollback();
                } catch {}
                throw error;
            }
            const verifyTx = await db.begin('readonly');
            let afterCommitName: string | null = null;
            let oldLookupAfterCommit: string | null = null;
            try {
                const afterCommit = await verifyTx.getByIndex(
                    'users',
                    'byEmail',
                    window.moyodb.indexKey('after@example.com')
                );
                const oldLookup = await verifyTx.getByIndex(
                    'users',
                    'byEmail',
                    window.moyodb.indexKey('before@example.com')
                );
                afterCommitName = afterCommit
                    ? decodeJson<{
                          name: string;
                      }>(afterCommit).name
                    : null;
                oldLookupAfterCommit = oldLookup
                    ? decodeJson<{
                          name: string;
                      }>(oldLookup).name
                    : null;
            } finally {
                await verifyTx.rollback();
            }
            await db.clearStore('users');
            const clearedTx = await db.begin('readonly');
            let clearedRows = 0;
            try {
                for await (const _row of clearedTx.scanByIndex('users', 'byEmail', {})) {
                    clearedRows += 1;
                }
            } finally {
                await clearedTx.rollback();
            }
            await db.put('users', encode('u:2'), window.moyodb.jsonEncode({ email: 'drop@example.com', name: 'Drop' }));
            await db.dropStore('users');
            let postDropErrorName: string | null = null;
            const postDropTx = await db.begin('readonly');
            try {
                await postDropTx.getByIndex('users', 'byEmail', window.moyodb.indexKey('drop@example.com'));
            } catch (error) {
                postDropErrorName = error instanceof Error ? error.name : String(error);
            } finally {
                await postDropTx.rollback();
            }
            return {
                beforeCommitName,
                oldLookupBeforeCommit,
                afterCommitName,
                oldLookupAfterCommit,
                clearedRows,
                storesAfterDrop: await db.listStores(),
                statsAfterDrop: await db.stats(),
                postDropErrorName
            };
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.beforeCommitName).toBe('After');
    expect(result.oldLookupBeforeCommit).toBeNull();
    expect(result.afterCommitName).toBe('After');
    expect(result.oldLookupAfterCommit).toBeNull();
    expect(result.clearedRows).toBe(0);
    expect(result.storesAfterDrop).toEqual([]);
    expect(result.statsAfterDrop.store_count).toBe(0);
    expect(result.postDropErrorName).toBe('StoreNotFoundError');
});
test('unique managed indexes reject duplicates without disturbing committed state', async ({ page }) => {
    const dbName = uniqueDbName('indexes-unique');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const encode = window.moyodb.utf8Encode;
        const decode = window.moyodb.utf8Decode;
        const decodeJson = window.moyodb.jsonDecode as <T>(bytes: Uint8Array) => T;
        const db = await window.moyodb.openDB(name, {
            version: 1,
            indexes: [{ store: 'users', name: 'byEmail', keyPath: 'email', unique: true }],
            migrate: async ({ db }) => {
                await db.createStore('users');
            }
        });
        try {
            await db.put(
                'users',
                encode('u:1'),
                window.moyodb.jsonEncode({ email: 'dup@example.com', name: 'Winner' })
            );
            const tx = await db.begin('readwrite');
            let errorName: string | null = null;
            let errorMessage: string | null = null;
            try {
                await tx.put(
                    'users',
                    encode('u:2'),
                    window.moyodb.jsonEncode({ email: 'dup@example.com', name: 'Loser' })
                );
            } catch (error) {
                errorName = error instanceof Error ? error.name : String(error);
                errorMessage = error instanceof Error ? error.message : String(error);
            } finally {
                await tx.rollback();
            }
            const winner = await db.get('users', encode('u:1'));
            const allRows = await db.scan('users');
            const lookupTx = await db.begin('readonly');
            try {
                const indexed = await lookupTx.getByIndex(
                    'users',
                    'byEmail',
                    window.moyodb.indexKey('dup@example.com')
                );
                return {
                    errorName,
                    errorMessage,
                    rowCount: allRows.length,
                    rowKeys: allRows.map((row) => decode(row.key)),
                    winnerName: winner
                        ? decodeJson<{
                              name: string;
                          }>(winner).name
                        : null,
                    indexedName: indexed
                        ? decodeJson<{
                              name: string;
                          }>(indexed).name
                        : null
                };
            } finally {
                await lookupTx.rollback();
            }
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.errorName).toBe('UniqueIndexConstraintError');
    expect(result.errorMessage).toContain('unique index constraint violation');
    expect(result.rowCount).toBe(1);
    expect(result.rowKeys).toEqual(['u:1']);
    expect(result.winnerName).toBe('Winner');
    expect(result.indexedName).toBe('Winner');
});
test('same-version reopen rejects index catalog drift', async ({ page }) => {
    const dbName = uniqueDbName('indexes-version-drift');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const initial = await window.moyodb.openDB(name, {
            version: 1,
            indexes: [{ store: 'users', name: 'byEmail', keyPath: 'email' }],
            migrate: async ({ db }) => {
                await db.createStore('users');
            }
        });
        await initial.close();
        let errorName: string | null = null;
        let errorMessage: string | null = null;
        try {
            await window.moyodb.openDB(name, {
                version: 1,
                indexes: [{ store: 'users', name: 'byHandle', keyPath: 'handle' }],
                migrate: async () => {
                    throw new Error('should not run');
                }
            });
        } catch (error) {
            errorName = error instanceof Error ? error.name : String(error);
            errorMessage = error instanceof Error ? error.message : String(error);
        }
        return { errorName, errorMessage };
    }, dbName);
    expect(result.errorName).toBe('VersionError');
    expect(result.errorMessage).toContain('requested index catalog does not match');
});
test('managed index catalog survives snapshot import, reset, and rebuild', async ({ page }) => {
    const sourceName = uniqueDbName('indexes-snapshot-source');
    const targetName = uniqueDbName('indexes-snapshot-target');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(
        async ({ source, target }) => {
            const encode = window.moyodb.utf8Encode;
            const decodeJson = window.moyodb.jsonDecode as <T>(bytes: Uint8Array) => T;
            const sourceDb = await window.moyodb.openDB(source, {
                version: 1,
                indexes: [{ store: 'users', name: 'byEmail', keyPath: 'email', unique: true }],
                migrate: async ({ db }) => {
                    await db.createStore('users');
                }
            });
            let snapshot: Uint8Array;
            try {
                await sourceDb.put(
                    'users',
                    encode('u:1'),
                    window.moyodb.jsonEncode({ email: 'alice@example.com', name: 'Alice' })
                );
                snapshot = await sourceDb.exportSnapshot();
            } finally {
                await sourceDb.close();
            }
            const targetDb = await window.moyodb.openDB(target);
            try {
                await targetDb.importSnapshot(snapshot);
                const importedTx = await targetDb.begin('readonly');
                let importedName: string | null = null;
                try {
                    const imported = await importedTx.getByIndex(
                        'users',
                        'byEmail',
                        window.moyodb.indexKey('alice@example.com')
                    );
                    importedName = imported
                        ? decodeJson<{
                              name: string;
                          }>(imported).name
                        : null;
                } finally {
                    await importedTx.rollback();
                }
                await targetDb.reset();
                await targetDb.put(
                    'users',
                    encode('u:2'),
                    window.moyodb.jsonEncode({ email: 'bob@example.com', name: 'Bob' })
                );
                await targetDb.rebuild();
                await targetDb.close();
                const reopened = await window.moyodb.openDB(target);
                try {
                    const reopenedTx = await reopened.begin('readonly');
                    try {
                        const rebuilt = await reopenedTx.getByIndex(
                            'users',
                            'byEmail',
                            window.moyodb.indexKey('bob@example.com')
                        );
                        return {
                            importedName,
                            rebuiltName: rebuilt
                                ? decodeJson<{
                                      name: string;
                                  }>(rebuilt).name
                                : null,
                            stores: await reopened.listStores(),
                            stats: await reopened.stats()
                        };
                    } finally {
                        await reopenedTx.rollback();
                    }
                } finally {
                    await reopened.close();
                }
            } catch (error) {
                try {
                    await targetDb.close();
                } catch {}
                throw error;
            }
        },
        { source: sourceName, target: targetName }
    );
    expect(result.importedName).toBe('Alice');
    expect(result.rebuiltName).toBe('Bob');
    expect(result.stores).toEqual(['users']);
    expect(result.stats.store_count).toBe(1);
});
