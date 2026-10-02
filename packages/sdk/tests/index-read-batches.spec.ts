import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';

test('paged compressed index reads preserve duplicate order, TTL filtering and old snapshots', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const compressionSupported = await page.evaluate(
        () => typeof CompressionStream === 'function' && typeof DecompressionStream === 'function'
    );
    test.skip(!compressionSupported, 'browser lacks compression streams');
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
