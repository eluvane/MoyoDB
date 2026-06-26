import { expect, test, type Page } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
async function requireCompressionStreams(page: Page) {
    const supported = await page.evaluate(() => {
        return typeof CompressionStream === 'function' && typeof DecompressionStream === 'function';
    });
    test.skip(!supported, 'browser lacks CompressionStream/DecompressionStream');
}
test('compressed stores decode transparently for get/scan and managed indexes', async ({ page }) => {
    const dbName = uniqueDbName('compression-store');
    await prepareMoyoDbPage(page);
    await requireCompressionStreams(page);
    const result = await page.evaluate(async (name) => {
        const encode = window.moyodb.utf8Encode;
        const decodeJson = window.moyodb.jsonDecode as <T>(bytes: Uint8Array) => T;
        const largeBody = 'moyodb compression payload '.repeat(512);
        const db = await window.moyodb.openDB(name, {
            version: 1,
            indexes: [{ store: 'docs', name: 'byType', keyPath: 'type' }],
            migrate: async ({ db }) => {
                await db.createStore('docs', { compression: 'gzip' });
            }
        });
        try {
            await db.put(
                'docs',
                encode('doc:1'),
                window.moyodb.jsonEncode({
                    type: 'article',
                    title: 'Large document',
                    body: largeBody
                })
            );
            const direct = await db.get('docs', encode('doc:1'));
            const rows = await db.scan('docs');
            const tx = await db.begin('readonly');
            try {
                const indexed = await tx.getByIndex('docs', 'byType', window.moyodb.indexKey('article'));
                return {
                    direct: direct
                        ? decodeJson<{
                              title: string;
                              body: string;
                          }>(direct)
                        : null,
                    scanCount: rows.length,
                    scannedTitle:
                        rows.length > 0
                            ? decodeJson<{
                                  title: string;
                              }>(rows[0]!.value).title
                            : null,
                    indexedTitle: indexed
                        ? decodeJson<{
                              title: string;
                          }>(indexed).title
                        : null
                };
            } finally {
                await tx.rollback();
            }
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result.direct?.title).toBe('Large document');
    expect(result.direct?.body.startsWith('moyodb compression payload')).toBe(true);
    expect(result.scanCount).toBe(1);
    expect(result.scannedTitle).toBe('Large document');
    expect(result.indexedTitle).toBe('Large document');
});
test('store-level compression shrinks large values but leaves small values effectively uncompressed', async ({
    page
}) => {
    const baseName = uniqueDbName('compression-threshold');
    await prepareMoyoDbPage(page);
    await requireCompressionStreams(page);
    const result = await page.evaluate(async (prefix) => {
        const encode = window.moyodb.utf8Encode;
        const bigPayload = window.moyodb.jsonEncode({
            kind: 'big',
            body: 'compressible-content-'.repeat(4096)
        });
        const smallPayload = window.moyodb.jsonEncode({
            kind: 'small',
            body: 'tiny'.repeat(12)
        });
        async function snapshotSize(name: string, compression: 'gzip' | false, value: Uint8Array) {
            const db = await window.moyodb.openDB(name);
            try {
                await db.createStore('docs', compression ? { compression } : {});
                await db.put('docs', encode('k'), value);
                const snapshot = await db.exportSnapshot();
                return snapshot.byteLength;
            } finally {
                await db.close();
            }
        }
        return {
            rawBig: await snapshotSize(`${prefix}-raw-big`, false, bigPayload),
            gzipBig: await snapshotSize(`${prefix}-gzip-big`, 'gzip', bigPayload),
            rawSmall: await snapshotSize(`${prefix}-raw-small`, false, smallPayload),
            gzipSmall: await snapshotSize(`${prefix}-gzip-small`, 'gzip', smallPayload)
        };
    }, baseName);
    expect(result.gzipBig).toBeLessThan(result.rawBig - 512);
    expect(result.gzipSmall).toBeGreaterThanOrEqual(result.rawSmall);
    expect(result.gzipSmall - result.rawSmall).toBeLessThan(96);
});
test('compressed snapshot export/import roundtrip restores values and indexes', async ({ page }) => {
    const source = uniqueDbName('compression-snapshot-source');
    const target = uniqueDbName('compression-snapshot-target');
    await prepareMoyoDbPage(page);
    await requireCompressionStreams(page);
    const result = await page.evaluate(
        async ({ sourceName, targetName }) => {
            const encode = window.moyodb.utf8Encode;
            const decodeJson = window.moyodb.jsonDecode as <T>(bytes: Uint8Array) => T;
            const body = 'snapshot-compression-body '.repeat(2048);
            const sourceDb = await window.moyodb.openDB(sourceName, {
                version: 1,
                indexes: [{ store: 'docs', name: 'byCategory', keyPath: 'category' }],
                migrate: async ({ db }) => {
                    await db.createStore('docs', { compression: 'gzip' });
                }
            });
            let plainSnapshot: Uint8Array;
            let compressedSnapshot: Uint8Array;
            try {
                await sourceDb.put(
                    'docs',
                    encode('doc:1'),
                    window.moyodb.jsonEncode({
                        category: 'guides',
                        title: 'Compression guide',
                        body
                    })
                );
                plainSnapshot = await sourceDb.exportSnapshot();
                compressedSnapshot = await sourceDb.exportSnapshot({ compression: 'deflate' });
            } finally {
                await sourceDb.close();
            }
            const targetDb = await window.moyodb.openDB(targetName);
            try {
                await targetDb.importSnapshot(compressedSnapshot);
                const rows = await targetDb.scan('docs');
                const tx = await targetDb.begin('readonly');
                try {
                    const indexed = await tx.getByIndex('docs', 'byCategory', window.moyodb.indexKey('guides'));
                    return {
                        plainSnapshotSize: plainSnapshot.byteLength,
                        compressedSnapshotSize: compressedSnapshot.byteLength,
                        stores: await targetDb.listStores(),
                        rowCount: rows.length,
                        restoredTitle:
                            rows.length > 0
                                ? decodeJson<{
                                      title: string;
                                  }>(rows[0]!.value).title
                                : null,
                        indexedTitle: indexed
                            ? decodeJson<{
                                  title: string;
                              }>(indexed).title
                            : null
                    };
                } finally {
                    await tx.rollback();
                }
            } finally {
                await targetDb.close();
            }
        },
        { sourceName: source, targetName: target }
    );
    expect(result.compressedSnapshotSize).toBeLessThan(result.plainSnapshotSize);
    expect(result.stores).toEqual(['docs']);
    expect(result.rowCount).toBe(1);
    expect(result.restoredTitle).toBe('Compression guide');
    expect(result.indexedTitle).toBe('Compression guide');
});

test('compressed snapshot import rejects oversized advertised output before inflating', async ({ page }) => {
    const source = uniqueDbName('compression-oversized-source');
    const target = uniqueDbName('compression-oversized-target');
    await prepareMoyoDbPage(page);
    await requireCompressionStreams(page);
    const result = await page.evaluate(
        async ({ sourceName, targetName }) => {
            const sourceDb = await window.moyodb.openDB(sourceName);
            let snapshot: Uint8Array;
            try {
                await sourceDb.createStore('docs');
                await sourceDb.put('docs', window.moyodb.utf8Encode('k'), window.moyodb.utf8Encode('v'));
                snapshot = await sourceDb.exportSnapshot({ compression: 'gzip' });
            } finally {
                await sourceDb.close();
            }

            new DataView(snapshot.buffer, snapshot.byteOffset, snapshot.byteLength).setUint32(10, 0xffff_ffff, true);

            const targetDb = await window.moyodb.openDB(targetName);
            try {
                await targetDb.importSnapshot(snapshot);
                return null;
            } catch (error) {
                return {
                    name: error instanceof Error ? error.name : null,
                    message: error instanceof Error ? error.message : String(error)
                };
            } finally {
                await targetDb.close();
            }
        },
        { sourceName: source, targetName: target }
    );
    expect(result?.name).toBe('CorruptionError');
    expect(result?.message).toContain('exceeding the 268435456 byte limit');
});
