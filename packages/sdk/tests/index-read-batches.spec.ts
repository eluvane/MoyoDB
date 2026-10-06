import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, requireCompressionStreams, uniqueDbName } from './support';
import type * as RegistryModule from '../src/registry';

test('secondary pages bound decoded bytes and resume before an unreturned large candidate', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const sdk = window.moyodb;
        const db = await sdk.openDB(name, {
            requestPersistence: false,
            version: 1,
            indexes: [{ store: 'docs', name: 'byGroup', keyPath: 'group' }],
            migrate: async ({ db }) => {
                await db.createStore('docs', { compression: 'snappy' });
            }
        });
        const modulePath = '/src/registry.ts';
        const registry = (await import(modulePath)) as typeof RegistryModule;
        const entry = await registry.acquireDbWorker(name, { requestPersistence: false });
        try {
            const seed = await db.begin('readwrite');
            await seed.putMany(
                'docs',
                Array.from({ length: 20 }, (_, id) => [
                    sdk.utf8Encode(String(id).padStart(3, '0')),
                    sdk.jsonEncode({ id, group: 'G', body: 'x'.repeat(4096) })
                ])
            );
            await seed.commit();
            const tx = await db.begin('readonly');
            try {
                const txId = (tx as unknown as { internalId(): number }).internalId();
                const ids: number[] = [];
                const sizes: number[] = [];
                let cursor: Uint8Array | null = null;
                for (;;) {
                    const page = await entry.proxy.scanByIndexPage(txId, 'docs', 'byGroup', {}, cursor, 256, 6000);
                    sizes.push(page.rows.reduce((size, row) => size + 8 + row.key.length + row.value.length, 4));
                    ids.push(...page.rows.map((row) => sdk.jsonDecode<{ id: number }>(row.value).id));
                    cursor = page.cursor;
                    if (cursor === null) break;
                }
                const reverse: number[] = [];
                for await (const [, value] of tx.scanByIndex(
                    'docs',
                    'byGroup',
                    { reverse: true, limit: 3 },
                    { maxRows: 256, maxBytes: 6000 }
                )) {
                    reverse.push(sdk.jsonDecode<{ id: number }>(value).id);
                }
                let oversized = '';
                try {
                    for await (const row of tx.scanByIndex('docs', 'byGroup', {}, { maxBytes: 100 })) {
                        throw new Error(`oversized indexed row returned ${row[0].length} key bytes`);
                    }
                } catch (error) {
                    oversized = (error as Error).name;
                }
                return { ids, sizes, reverse, oversized };
            } finally {
                await tx.rollback();
            }
        } finally {
            await registry.releaseDbWorker(entry);
            await db.destroy();
        }
    }, uniqueDbName('index-byte-budget'));
    expect(result.ids).toEqual(Array.from({ length: 20 }, (_, id) => id));
    expect(result.sizes.every((bytes) => bytes <= 6000)).toBe(true);
    expect(result.sizes.length).toBe(20);
    expect(result.reverse).toEqual([19, 18, 17]);
    expect(result.oversized).toBe('ValueTooLargeError');
});

test('indexed SQL LIMIT one stops before a later malformed persisted row', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const sdk = window.moyodb;
        const indexes = [{ store: 'docs', name: 'byGroup', keyPath: 'group_name' }];
        const db = await sdk.openDB(name, { version: 1, indexes, migrate: () => {} });
        const sql = sdk.createSqlClient(db, { indexes });
        try {
            await sql.execute(
                'CREATE TABLE docs (id INTEGER PRIMARY KEY, group_name TEXT NOT NULL, body TEXT NOT NULL)'
            );
            await sql.execute('INSERT INTO docs VALUES (?, ?, ?), (?, ?, ?)', [1, 'G', 'first', 2, 'G', 'second']);
            const stored = await db.scan('docs');
            await db.put(
                'docs',
                stored[1].key,
                sdk.jsonEncode({ id: 2, group_name: 'G', body: 'second', extra: true })
            );
            const first = await sql.execute("SELECT id FROM docs WHERE group_name = 'G' LIMIT 1");
            let laterError = '';
            try {
                await sql.execute("SELECT id FROM docs WHERE group_name = 'G' LIMIT 2");
            } catch (error) {
                laterError = (error as Error).name;
            }
            return { first, laterError, active: (await db.stats()).active_txns };
        } finally {
            await db.destroy();
        }
    }, uniqueDbName('sql-index-limit-one'));
    expect(result.first.rows).toEqual([{ id: 1 }]);
    expect(result.first.plan?.access).toBe('index-lookup');
    expect(result.first.plan?.sort).toBe('none');
    expect(result.laterError).toBe('SerializationError');
    expect(result.active).toBe(0);
});

test('paged compressed index reads preserve duplicate order, TTL filtering and old snapshots', async ({ page }) => {
    await prepareMoyoDbPage(page);
    await requireCompressionStreams(page, 'browser lacks compression streams');
    const result = await page.evaluate(async (name) => {
        const sdk = window.moyodb;
        const db = await sdk.openDB(name, {
            version: 1,
            indexes: [{ store: 'docs', name: 'byGroup', keyPath: 'group' }],
            migrate: async ({ db }) => {
                await db.createStore('docs', { compression: 'gzip' });
            }
        });
        const body = 'compressed indexed document '.repeat(80);
        const keyFor = (id: number) => sdk.utf8Encode(String(id).padStart(4, '0'));
        const value = (id: number, group = 'G') => sdk.jsonEncode({ id, group, body });
        try {
            const seed = await db.begin('readwrite');
            try {
                for (let id = 0; id < 300; id += 1) {
                    await seed.put('docs', keyFor(id), value(id), id < 24 ? { ttl: 0 } : {});
                }
                await seed.commit();
            } finally {
                await seed.rollback().catch(() => undefined);
            }
            const oldReader = await db.begin('readonly');
            try {
                const readIds = async (reverse: boolean) => {
                    const ids: number[] = [];
                    for await (const [key, bytes] of oldReader.scanByIndex('docs', 'byGroup', {
                        ...sdk.prefixRange(sdk.indexKey('G')),
                        reverse,
                        limit: 270
                    })) {
                        const document = sdk.jsonDecode<{ id: number; group: string; body: string }>(bytes);
                        if (
                            document.body !== body ||
                            document.group !== 'G' ||
                            Number(sdk.utf8Decode(key)) !== document.id
                        ) {
                            throw new Error('indexed row changed its key or decompressed document');
                        }
                        ids.push(document.id);
                    }
                    return ids;
                };
                const forward = await readIds(false);
                const reverse = await readIds(true);
                const exact = await oldReader.getByIndex('docs', 'byGroup', sdk.indexKey('G'));
                const writer = await db.begin('readwrite');
                try {
                    await writer.put('docs', keyFor(24), value(24, 'H'));
                    const liveIds: number[] = [];
                    for await (const [, bytes] of writer.scanByIndex('docs', 'byGroup', {
                        ...sdk.prefixRange(sdk.indexKey('G')),
                        limit: 2
                    })) {
                        liveIds.push(sdk.jsonDecode<{ id: number }>(bytes).id);
                    }
                    await writer.commit();
                    const oldExact = await oldReader.getByIndex('docs', 'byGroup', sdk.indexKey('G'));
                    const newReader = await db.begin('readonly');
                    try {
                        const newExact = await newReader.getByIndex('docs', 'byGroup', sdk.indexKey('G'));
                        return {
                            forward,
                            reverse,
                            exact: exact === null ? null : sdk.jsonDecode<{ id: number }>(exact).id,
                            liveIds,
                            oldExact: oldExact === null ? null : sdk.jsonDecode<{ id: number }>(oldExact).id,
                            newExact: newExact === null ? null : sdk.jsonDecode<{ id: number }>(newExact).id
                        };
                    } finally {
                        await newReader.rollback();
                    }
                } finally {
                    await writer.rollback().catch(() => undefined);
                }
            } finally {
                await oldReader.rollback();
            }
        } finally {
            await db.close();
        }
    }, uniqueDbName('index-read-batches'));
    expect(result.forward).toEqual(Array.from({ length: 270 }, (_, index) => index + 24));
    expect(result.reverse).toEqual(Array.from({ length: 270 }, (_, index) => 299 - index));
    expect(result.exact).toBe(24);
    expect(result.liveIds).toEqual([25, 26]);
    expect(result.oldExact).toBe(24);
    expect(result.newExact).toBe(25);
});
