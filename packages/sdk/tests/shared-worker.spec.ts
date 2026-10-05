import { expect, test, type BrowserContext, type Page } from '@playwright/test';
import type { DB, Transaction } from '../src/types';
import { prepareMoyoDbPage, uniqueDbName } from './support';

interface SharedTestState {
    db: DB;
    tx?: Transaction;
    events?: string[];
    migrationEntered?: boolean;
    finishMigration?: () => void;
    opening?: Promise<DB>;
    migrations?: number;
}

async function prepareSharedPages(context: BrowserContext): Promise<[Page, Page]> {
    const first = await context.newPage();
    const second = await context.newPage();
    await Promise.all([prepareMoyoDbPage(first), prepareMoyoDbPage(second)]);
    test.skip(!(await first.evaluate(() => typeof SharedWorker === 'function')), 'SharedWorker is unavailable');
    return [first, second];
}

async function openShared(page: Page, name: string): Promise<void> {
    await page.evaluate(async (dbName) => {
        const state = window as unknown as SharedTestState;
        state.db = await window.moyodb.openDB(dbName, { workerMode: 'shared', requestPersistence: false });
    }, name);
}

test('shared owner delivers changes across pages and survives a client close', async ({ context }) => {
    const [first, second] = await prepareSharedPages(context);
    const name = uniqueDbName('shared-pages');
    await openShared(first, name);
    await first.evaluate(() => (window as unknown as SharedTestState).db.createStore('kv'));
    await openShared(second, name);
    await second.evaluate(() => {
        const state = window as unknown as SharedTestState;
        state.events = [];
        state.db.subscribe('kv', (store, changes) => state.events!.push(`${store}:${changes[0].kind}`));
    });
    await first.evaluate(async () => {
        const state = window as unknown as SharedTestState;
        await state.db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
        await state.db.close();
    });
    await expect.poll(() => second.evaluate(() => (window as unknown as SharedTestState).events)).toEqual(['kv:put']);
    expect(
        await second.evaluate(async () => {
            const state = window as unknown as SharedTestState;
            const value = await state.db.get('kv', window.moyodb.utf8Encode('a'));
            await state.db.put('kv', window.moyodb.utf8Encode('b'), window.moyodb.utf8Encode('2'));
            await state.db.close();
            return value ? window.moyodb.utf8Decode(value) : null;
        })
    ).toBe('1');
    await first.evaluate((dbName) => window.moyodb.deleteDB(dbName), name);
});

test('shared transaction IDs are isolated and client close rolls back only its client', async ({ context }) => {
    const [first, second] = await prepareSharedPages(context);
    const name = uniqueDbName('shared-tx-isolation');
    await first.evaluate(async (dbName) => {
        const db = await window.moyodb.openDB(dbName, { requestPersistence: false });
        await db.createStore('kv');
        await db.put('kv', window.moyodb.utf8Encode('stable'), window.moyodb.utf8Encode('value'));
        await db.close();
    }, name);
    await Promise.all([openShared(first, name), openShared(second, name)]);
    const ids = await Promise.all(
        [first, second].map((page, index) =>
            page.evaluate(
                async (mode) => {
                    const state = window as unknown as SharedTestState;
                    state.tx = await state.db.begin(mode);
                    return (state.tx as unknown as { internalId(): number }).internalId();
                },
                index === 0 ? ('readwrite' as const) : ('readonly' as const)
            )
        )
    );
    expect(ids).toEqual([1, 1]);
    await first.evaluate(async () => {
        const state = window as unknown as SharedTestState;
        await state.tx!.put('kv', window.moyodb.utf8Encode('uncommitted'), window.moyodb.utf8Encode('discard'));
    });
    await first.evaluate(() => (window as unknown as SharedTestState).db.close());
    await expect
        .poll(() => second.evaluate(async () => (await (window as unknown as SharedTestState).db.stats()).active_txns))
        .toBe(1);
    expect(
        await second.evaluate(async () => {
            const state = window as unknown as SharedTestState;
            const stable = await state.tx!.get('kv', window.moyodb.utf8Encode('stable'));
            await state.tx!.rollback();
            const absent = await state.db.get('kv', window.moyodb.utf8Encode('uncommitted'));
            await state.db.destroy();
            return { stable: stable ? window.moyodb.utf8Decode(stable) : null, absent };
        })
    ).toEqual({ stable: 'value', absent: null });
});

test('shared destroy rejects a live peer and retains both handles', async ({ context }) => {
    const [first, second] = await prepareSharedPages(context);
    const name = uniqueDbName('shared-destroy-busy');
    await Promise.all([openShared(first, name), openShared(second, name)]);
    expect(
        await first.evaluate(async () => {
            const state = window as unknown as SharedTestState;
            try {
                await state.db.destroy();
                return 'NO_ERROR';
            } catch (error) {
                return (error as Error).name;
            }
        })
    ).toBe('DatabaseBusyError');
    expect(await first.evaluate(() => (window as unknown as SharedTestState).db.listStores())).toEqual([]);
    expect(await second.evaluate(() => (window as unknown as SharedTestState).db.listStores())).toEqual([]);
    await second.evaluate(() => (window as unknown as SharedTestState).db.close());
    await first.evaluate(() => (window as unknown as SharedTestState).db.destroy());
});

test('shared owner page unload reopens storage for the peer and invalidates old transactions', async ({ context }) => {
    const [first, second] = await prepareSharedPages(context);
    const name = uniqueDbName('shared-owner-handoff');
    await openShared(first, name);
    await first.evaluate(async () => {
        const state = window as unknown as SharedTestState;
        await state.db.createStore('kv');
        await state.db.put('kv', window.moyodb.utf8Encode('stable'), window.moyodb.utf8Encode('value'));
    });
    await openShared(second, name);
    await second.evaluate(async () => {
        const state = window as unknown as SharedTestState;
        state.tx = await state.db.begin('readonly');
    });
    await first.close();
    await expect
        .poll(() =>
            second.evaluate(async () => {
                try {
                    await (window as unknown as SharedTestState).tx!.get('kv', window.moyodb.utf8Encode('stable'));
                    return 'NO_ERROR';
                } catch (error) {
                    return (error as Error).name;
                }
            })
        )
        .toBe('TransactionClosedError');
    expect(
        await second.evaluate(async () => {
            const state = window as unknown as SharedTestState;
            const stable = await state.db.get('kv', window.moyodb.utf8Encode('stable'));
            await state.db.put('kv', window.moyodb.utf8Encode('after'), window.moyodb.utf8Encode('handoff'));
            await state.db.destroy();
            return stable ? window.moyodb.utf8Decode(stable) : null;
        })
    ).toBe('value');
});

test('shared clients can hold overlapping writers and receive a deterministic stale commit conflict', async ({
    context
}) => {
    const [first, second] = await prepareSharedPages(context);
    const name = uniqueDbName('shared-writers');
    await openShared(first, name);
    await first.evaluate(() => (window as unknown as SharedTestState).db.createStore('kv'));
    await openShared(second, name);
    await Promise.all(
        [first, second].map((page, index) =>
            page.evaluate(async (value) => {
                const state = window as unknown as SharedTestState;
                state.tx = await state.db.begin('readwrite');
                await state.tx.put('kv', window.moyodb.utf8Encode('key'), window.moyodb.utf8Encode(value));
            }, String(index))
        )
    );
    await first.evaluate(() => (window as unknown as SharedTestState).tx!.commit());
    expect(
        await second.evaluate(async () => {
            const state = window as unknown as SharedTestState;
            try {
                await state.tx!.commit();
                return 'NO_ERROR';
            } catch (error) {
                return (error as Error).name;
            }
        })
    ).toBe('TransactionConflictError');
    expect(
        await second.evaluate(async () => {
            const state = window as unknown as SharedTestState;
            const value = await state.db.get('kv', window.moyodb.utf8Encode('key'));
            return {
                value: value ? window.moyodb.utf8Decode(value) : null,
                active: (await state.db.stats()).active_txns
            };
        })
    ).toEqual({ value: '0', active: 0 });
    await second.evaluate(() => (window as unknown as SharedTestState).db.close());
    await first.evaluate(() => (window as unknown as SharedTestState).db.destroy());
});

test('shared schema migration has one owner and other page rechecks committed version', async ({ context }) => {
    const [first, second] = await prepareSharedPages(context);
    const name = uniqueDbName('shared-migration');
    await first.evaluate((dbName) => {
        const state = window as unknown as SharedTestState;
        state.opening = window.moyodb.openDB(dbName, {
            workerMode: 'shared',
            requestPersistence: false,
            version: 1,
            migrate: async ({ db }) => {
                state.migrationEntered = true;
                await new Promise<void>((resolve) => {
                    state.finishMigration = resolve;
                });
                await db.createStore('migrated');
            }
        });
    }, name);
    await expect.poll(() => first.evaluate(() => (window as unknown as SharedTestState).migrationEntered)).toBe(true);
    await second.evaluate((dbName) => {
        const state = window as unknown as SharedTestState;
        state.migrations = 0;
        state.opening = window.moyodb.openDB(dbName, {
            workerMode: 'shared',
            requestPersistence: false,
            version: 1,
            migrate: async () => {
                state.migrations! += 1;
            }
        });
    }, name);
    await first.evaluate(() => (window as unknown as SharedTestState).finishMigration!());
    await Promise.all(
        [first, second].map((page) =>
            page.evaluate(async () => {
                const state = window as unknown as SharedTestState;
                state.db = await state.opening!;
            })
        )
    );
    expect(
        await Promise.all(
            [first, second].map((page) =>
                page.evaluate(async () => {
                    const state = window as unknown as SharedTestState;
                    return {
                        version: await state.db.getVersion(),
                        stores: await state.db.listStores(),
                        migrations: state.migrations ?? 1
                    };
                })
            )
        )
    ).toEqual([
        { version: 1, stores: ['migrated'], migrations: 1 },
        { version: 1, stores: ['migrated'], migrations: 0 }
    ]);
    await second.evaluate(() => (window as unknown as SharedTestState).db.close());
    await first.evaluate(() => (window as unknown as SharedTestState).db.destroy());
});

test('shared maintenance invalidates peer transactions without closing its handle', async ({ context }) => {
    const [first, second] = await prepareSharedPages(context);
    const name = uniqueDbName('shared-maintenance');
    await Promise.all([openShared(first, name), openShared(second, name)]);
    await second.evaluate(async () => {
        const state = window as unknown as SharedTestState;
        state.tx = await state.db.begin('readonly');
    });
    await first.evaluate(() => (window as unknown as SharedTestState).db.reset());
    expect(
        await second.evaluate(async () => {
            const state = window as unknown as SharedTestState;
            let errorName = 'NO_ERROR';
            try {
                await state.tx!.get('kv', new Uint8Array());
            } catch (error) {
                errorName = (error as Error).name;
            }
            return { errorName, stores: await state.db.listStores() };
        })
    ).toEqual({ errorName: 'TransactionClosedError', stores: [] });
    await second.evaluate(() => (window as unknown as SharedTestState).db.close());
    await first.evaluate(() => (window as unknown as SharedTestState).db.destroy());
});

test('shared ordinary opens allow active peer transactions and schema upgrades reject them cleanly', async ({
    context
}) => {
    const [first, second] = await prepareSharedPages(context);
    const name = uniqueDbName('shared-active-schema');
    await openShared(first, name);
    await first.evaluate(async () => {
        const state = window as unknown as SharedTestState;
        await state.db.createStore('kv');
        state.tx = await state.db.begin('readonly');
    });
    await openShared(second, name);
    await second.evaluate(() => (window as unknown as SharedTestState).db.close());
    expect(
        await second.evaluate(async (dbName) => {
            try {
                await window.moyodb.openDB(dbName, {
                    workerMode: 'shared',
                    requestPersistence: false,
                    version: 1,
                    migrate: ({ db }) => db.createStore('new-store')
                });
                return 'NO_ERROR';
            } catch (error) {
                return (error as Error).name;
            }
        }, name)
    ).toBe('DatabaseBusyError');
    expect(
        await first.evaluate(async () => {
            const state = window as unknown as SharedTestState;
            const value = await state.tx!.get('kv', new Uint8Array());
            await state.tx!.rollback();
            return { value, version: await state.db.getVersion(), active: (await state.db.stats()).active_txns };
        })
    ).toEqual({ value: null, version: 0, active: 0 });
    await second.evaluate(async (dbName) => {
        const state = window as unknown as SharedTestState;
        state.db = await window.moyodb.openDB(dbName, {
            workerMode: 'shared',
            requestPersistence: false,
            version: 1,
            migrate: ({ db }) => db.createStore('new-store')
        });
        await state.db.close();
    }, name);
    expect(await first.evaluate(() => (window as unknown as SharedTestState).db.getVersion())).toBe(1);
    await first.evaluate(() => (window as unknown as SharedTestState).db.destroy());
});

test('shared storage worker crash invalidates every client and permits recovery', async ({ context }) => {
    const [first, second] = await prepareSharedPages(context);
    const name = uniqueDbName('shared-crash');
    await Promise.all([openShared(first, name), openShared(second, name)]);
    await second.evaluate(async () => {
        const state = window as unknown as SharedTestState;
        state.tx = await state.db.begin('readonly');
    });
    expect(await first.evaluate((dbName) => window.moyodb.unsafeDebugCrashWorker(dbName), name)).toBe(true);
    await expect
        .poll(() =>
            second.evaluate(async () => {
                try {
                    await (window as unknown as SharedTestState).db.listStores();
                    return 'NO_ERROR';
                } catch (error) {
                    return (error as Error).name;
                }
            })
        )
        .toBe('DatabaseClosedError');
    expect(
        await second.evaluate(async () => {
            try {
                await (window as unknown as SharedTestState).tx!.commit();
                return 'NO_ERROR';
            } catch (error) {
                return (error as Error).name;
            }
        })
    ).toBe('TransactionClosedError');
    await openShared(second, name);
    await second.evaluate(() => (window as unknown as SharedTestState).db.destroy());
});

test('explicit shared mode fails closed when SharedWorker is unavailable', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const errorName = await page.evaluate(async (name) => {
        const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'SharedWorker');
        Object.defineProperty(globalThis, 'SharedWorker', { configurable: true, value: undefined });
        try {
            await window.moyodb.openDB(name, { workerMode: 'shared', requestPersistence: false });
            return 'NO_ERROR';
        } catch (error) {
            return (error as Error).name;
        } finally {
            if (descriptor) Object.defineProperty(globalThis, 'SharedWorker', descriptor);
        }
    }, uniqueDbName('shared-unavailable'));
    expect(errorName).toBe('UnsupportedPlatformError');
});
