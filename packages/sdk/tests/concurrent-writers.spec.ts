import { expect, test } from '@playwright/test';
import type * as SDK from '../src/index';
import { prepareMoyoDbPage, uniqueDbName } from './support';

test('overlapping writers keep snapshots and retry after a typed commit conflict', async ({ page }) => {
    const dbName = uniqueDbName('concurrent-writers');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const sdkPath = '/src/index.ts';
        const { TransactionConflictError, TransactionClosedError } = (await import(sdkPath)) as typeof SDK;
        const options = { workerMode: 'dedicated' as const, requestPersistence: false };
        const db = await window.moyodb.openDB(name, options);
        const encode = window.moyodb.utf8Encode;
        const decode = (value: Uint8Array | null) => (value === null ? null : window.moyodb.utf8Decode(value));
        const failure = async (operation: () => Promise<unknown>) => {
            try {
                await operation();
                return { name: 'NO_ERROR', conflict: false, closed: false };
            } catch (error) {
                return {
                    name: error instanceof Error ? error.name : String(error),
                    conflict: error instanceof TransactionConflictError,
                    closed: error instanceof TransactionClosedError
                };
            }
        };
        try {
            await db.createStore('kv');
            await db.put('kv', encode('a'), encode('base'));
            const [first, stale, other, reader] = await Promise.all([
                db.begin('readwrite'),
                db.begin('readwrite'),
                db.begin('readwrite'),
                db.begin('readonly')
            ]);
            await first.put('kv', encode('a'), encode('first'));
            await stale.put('kv', encode('b'), encode('second'));
            await other.put('kv', encode('private'), encode('other'));
            const privateValues = {
                first: decode(await first.get('kv', encode('a'))),
                staleA: decode(await stale.get('kv', encode('a'))),
                staleB: decode(await stale.get('kv', encode('b'))),
                firstB: decode(await first.get('kv', encode('b'))),
                readerA: decode(await reader.get('kv', encode('a'))),
                readerB: decode(await reader.get('kv', encode('b')))
            };
            await first.commit();
            const conflict = await failure(() => stale.commit());
            const closedOperations = await Promise.all([
                failure(() => stale.get('kv', encode('b'))),
                failure(() => stale.put('kv', encode('b'), encode('closed'))),
                failure(() => stale.commit()),
                failure(() => stale.rollback())
            ]);
            await other.put('kv', encode('private'), encode('still-open'));
            const otherValue = decode(await other.get('kv', encode('private')));
            const activeAfterConflict = (await db.stats()).active_txns;
            const snapshotAfterConflict = {
                a: decode(await reader.get('kv', encode('a'))),
                b: decode(await reader.get('kv', encode('b')))
            };
            await other.rollback();

            const retry = await db.begin('readwrite');
            const retryBaseline = {
                a: decode(await retry.get('kv', encode('a'))),
                b: decode(await retry.get('kv', encode('b')))
            };
            await retry.put('kv', encode('b'), encode('second'));
            await retry.commit();
            const snapshotAfterRetry = {
                a: decode(await reader.get('kv', encode('a'))),
                b: decode(await reader.get('kv', encode('b')))
            };
            await reader.rollback();
            const activeAtEnd = (await db.stats()).active_txns;
            await db.close();

            const reopened = await window.moyodb.openDB(name, options);
            try {
                return {
                    privateValues,
                    conflict,
                    closedOperations,
                    otherValue,
                    activeAfterConflict,
                    snapshotAfterConflict,
                    retryBaseline,
                    snapshotAfterRetry,
                    activeAtEnd,
                    persisted: {
                        a: decode(await reopened.get('kv', encode('a'))),
                        b: decode(await reopened.get('kv', encode('b'))),
                        private: decode(await reopened.get('kv', encode('private')))
                    }
                };
            } finally {
                await reopened.close();
            }
        } finally {
            await db.close();
        }
    }, dbName);

    expect(result.privateValues).toEqual({
        first: 'first',
        staleA: 'base',
        staleB: 'second',
        firstB: null,
        readerA: 'base',
        readerB: null
    });
    expect(result.conflict).toEqual({ name: 'TransactionConflictError', conflict: true, closed: false });
    expect(result.closedOperations).toEqual(
        Array.from({ length: 4 }, () => ({ name: 'TransactionClosedError', conflict: false, closed: true }))
    );
    expect(result.otherValue).toBe('still-open');
    expect(result.activeAfterConflict).toBe(2);
    expect(result.snapshotAfterConflict).toEqual({ a: 'base', b: null });
    expect(result.retryBaseline).toEqual({ a: 'first', b: null });
    expect(result.snapshotAfterRetry).toEqual({ a: 'base', b: null });
    expect(result.activeAtEnd).toBe(0);
    expect(result.persisted).toEqual({ a: 'first', b: 'second', private: null });
});
