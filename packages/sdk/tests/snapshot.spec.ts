import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';
test('exportSnapshot/importSnapshot roundtrip restores database state', async ({ page }) => {
    const source = uniqueDbName('snapshot-source');
    const target = uniqueDbName('snapshot-target');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(
        async ({ sourceName, targetName }) => {
            const sourceDb = await window.moyodb.openDB(sourceName);
            let snapshot: Uint8Array;
            try {
                await sourceDb.createStore('alpha');
                await sourceDb.createStore('empty');
                await sourceDb.put('alpha', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
                await sourceDb.put('alpha', window.moyodb.utf8Encode('b'), window.moyodb.utf8Encode('2'));
                snapshot = await sourceDb.exportSnapshot();
            } finally {
                await sourceDb.close();
            }
            const targetDb = await window.moyodb.openDB(targetName);
            try {
                await targetDb.createStore('junk');
                await targetDb.put('junk', window.moyodb.utf8Encode('stale'), window.moyodb.utf8Encode('value'));
                await targetDb.importSnapshot(snapshot);
                const alphaRows = await targetDb.scan('alpha');
                const emptyRows = await targetDb.scan('empty');
                let junkErrorName: string | null = null;
                try {
                    await targetDb.get('junk', window.moyodb.utf8Encode('stale'));
                } catch (error) {
                    junkErrorName = error instanceof Error ? error.name : String(error);
                }
                return {
                    alphaRows: alphaRows.map((row) => ({
                        key: window.moyodb.utf8Decode(row.key),
                        value: window.moyodb.utf8Decode(row.value)
                    })),
                    emptyCount: emptyRows.length,
                    junkErrorName
                };
            } finally {
                await targetDb.close();
            }
        },
        { sourceName: source, targetName: target }
    );
    expect(result.alphaRows).toEqual([
        { key: 'a', value: '1' },
        { key: 'b', value: '2' }
    ]);
    expect(result.emptyCount).toBe(0);
    expect(result.junkErrorName).toBe('StoreNotFoundError');
});
test('importSnapshot rejects checksum mismatch', async ({ page }) => {
    const name = uniqueDbName('snapshot-checksum');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (dbName) => {
        const sourceDb = await window.moyodb.openDB(`${dbName}-source`);
        let snapshot: Uint8Array;
        try {
            await sourceDb.createStore('alpha');
            await sourceDb.put('alpha', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
            snapshot = await sourceDb.exportSnapshot();
        } finally {
            await sourceDb.close();
        }
        snapshot[32] ^= 0x5a;
        const targetDb = await window.moyodb.openDB(`${dbName}-target`);
        try {
            await targetDb.createStore('keep');
            await targetDb.put('keep', window.moyodb.utf8Encode('k'), window.moyodb.utf8Encode('v'));
            let errorName: string | null = null;
            let errorMessage: string | null = null;
            try {
                await targetDb.importSnapshot(snapshot);
            } catch (error) {
                errorName = error instanceof Error ? error.name : String(error);
                errorMessage = error instanceof Error ? error.message : String(error);
            }
            const kept = await targetDb.get('keep', window.moyodb.utf8Encode('k'));
            return {
                errorName,
                errorMessage,
                kept: kept ? window.moyodb.utf8Decode(kept) : null
            };
        } finally {
            await targetDb.close();
        }
    }, name);
    expect(result.errorName).toBe('CorruptionError');
    expect(result.errorMessage).toContain('checksum');
    expect(result.kept).toBe('v');
});

for (const failure of ['json', 'unique', 'oversize'] as const) {
    test(`importSnapshot preserves data and indexes after ${failure} rebuild failure and reopen`, async ({ page }) => {
        await prepareMoyoDbPage(page);
        const result = await page.evaluate(
            async ({ name, failure }) => {
                const encode = window.moyodb.utf8Encode;
                const source = await window.moyodb.openDB(`${name}-source`);
                let base: Uint8Array;
                try {
                    await source.createStore('docs');
                    const value =
                        failure === 'json'
                            ? encode('not-json')
                            : window.moyodb.jsonEncode({ value: failure === 'unique' ? 'same' : 'x'.repeat(1100) });
                    await source.put('docs', encode('one'), value);
                    if (failure === 'unique') await source.put('docs', encode('two'), value);
                    base = await source.exportSnapshot();
                } finally {
                    await source.close();
                }
                const manifest = encode(
                    JSON.stringify([{ store: 'docs', name: 'byValue', keyPath: 'value', unique: failure === 'unique' }])
                );
                const snapshot = new Uint8Array(base.length + manifest.length + 11);
                snapshot.set(base);
                snapshot.set(manifest, base.length);
                new DataView(snapshot.buffer).setUint32(base.length + manifest.length, manifest.length, true);
                snapshot.set(encode('BDBIDX1'), base.length + manifest.length + 4);
                let db = await window.moyodb.openDB(name, {
                    version: 1,
                    indexes: [{ store: 'keep', name: 'byValue', keyPath: 'value', unique: true }],
                    migrate: async ({ db }) => {
                        await db.createStore('keep');
                    }
                });
                try {
                    await db.put('keep', encode('old'), window.moyodb.jsonEncode({ value: 'kept' }));
                    const before = Array.from(await db.exportSnapshot());
                    let errorName: string | null = null;
                    try {
                        await db.importSnapshot(snapshot);
                    } catch (error) {
                        errorName = error instanceof Error ? error.name : String(error);
                    }
                    const after = Array.from(await db.exportSnapshot());
                    await db.close();
                    db = await window.moyodb.openDB(name);
                    const tx = await db.begin('readonly');
                    try {
                        const row = await tx.getByIndex('keep', 'byValue', window.moyodb.indexKey('kept'));
                        return {
                            errorName,
                            before,
                            after,
                            row: row ? window.moyodb.jsonDecode(row) : null,
                            indexes: await db.listIndexes?.()
                        };
                    } finally {
                        await tx.rollback();
                    }
                } finally {
                    await db.close();
                }
            },
            { name: uniqueDbName(`snapshot-${failure}`), failure }
        );
        expect(result.errorName).toBe(
            { json: 'SerializationError', unique: 'UniqueIndexConstraintError', oversize: 'KeyTooLargeError' }[failure]
        );
        expect(result.after).toEqual(result.before);
        expect(result.row).toEqual({ value: 'kept' });
        expect(result.indexes).toEqual([{ store: 'keep', name: 'byValue', keyPath: 'value', unique: true }]);
    });
}

test('snapshot number boundaries reject before publication and keep emitted txids usable', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const encode = window.moyodb.utf8Encode;
        const source = await window.moyodb.openDB(`${name}-source`);
        let base: Uint8Array;
        try {
            await source.createStore('docs');
            await source.put('docs', encode('new'), window.moyodb.jsonEncode({ value: 'value' }));
            base = await source.exportSnapshot();
        } finally {
            await source.close();
        }
        const patch = (txid: bigint, version: bigint): Uint8Array => {
            const bytes = base.slice();
            const view = new DataView(bytes.buffer);
            view.setBigUint64(32, txid, true);
            view.setBigUint64(40, version, true);
            view.setUint32(24, 0, true);
            let checksum = 0xffff_ffff;
            for (const byte of bytes) {
                checksum ^= byte;
                for (let bit = 0; bit < 8; bit++) checksum = (checksum >>> 1) ^ (checksum & 1 ? 0xedb8_8320 : 0);
            }
            view.setUint32(24, (checksum ^ 0xffff_ffff) >>> 0, true);
            return bytes;
        };
        const max = BigInt(Number.MAX_SAFE_INTEGER);
        const db = await window.moyodb.openDB(name, { changeFeed: { enabled: true } });
        try {
            await db.createStore('keep');
            await db.put('keep', encode('old'), encode('kept'));
            const before = Array.from(await db.exportSnapshot());
            const rejected: Array<{ error: string | null; same: boolean }> = [];
            for (const [txid, version] of [
                [max, 0n],
                [max + 1n, 0n],
                [0n, max + 1n]
            ]) {
                let error: string | null = null;
                try {
                    await db.importSnapshot(patch(txid, version));
                } catch (caught) {
                    error = caught instanceof Error ? caught.name : String(caught);
                }
                rejected.push({
                    error,
                    same: JSON.stringify(Array.from(await db.exportSnapshot())) === JSON.stringify(before)
                });
            }
            const edgeBase = patch(max - 1n, 0n);
            const manifest = encode(JSON.stringify([{ store: 'docs', name: 'byMissing', keyPath: 'missing' }]));
            const indexedEdge = new Uint8Array(edgeBase.length + manifest.length + 11);
            indexedEdge.set(edgeBase);
            indexedEdge.set(manifest, edgeBase.length);
            new DataView(indexedEdge.buffer).setUint32(edgeBase.length + manifest.length, manifest.length, true);
            indexedEdge.set(encode('BDBIDX1'), edgeBase.length + manifest.length + 4);
            let indexedEdgeError: string | null = null;
            try {
                await db.importSnapshot(indexedEdge);
            } catch (error) {
                indexedEdgeError = error instanceof Error ? error.name : String(error);
            }
            const indexedEdgePreserved =
                JSON.stringify(Array.from(await db.exportSnapshot())) === JSON.stringify(before);
            await db.importSnapshot(patch(max - 2n, max));
            const imported = (await db.stats()).last_committed_txid;
            await db.put('docs', encode('last'), encode('safe'));
            const feed = await db.changesSince(imported);
            const caughtUp = await db.changesSince(feed.latestTxId);
            let exhausted: string | null = null;
            try {
                await db.put('docs', encode('overflow'), encode('unsafe'));
            } catch (error) {
                exhausted = error instanceof Error ? error.name : String(error);
            }
            return {
                rejected,
                indexedEdgeError,
                indexedEdgePreserved,
                imported,
                latest: feed.latestTxId,
                caughtUp: caughtUp.latestTxId,
                exhausted,
                overflow: await db.get('docs', encode('overflow')),
                version: await db.getVersion()
            };
        } finally {
            await db.close();
        }
    }, uniqueDbName('snapshot-numbers'));
    expect(result.rejected).toEqual(Array.from({ length: 3 }, () => ({ error: 'SerializationError', same: true })));
    expect(result.indexedEdgeError).toBe('SerializationError');
    expect(result.indexedEdgePreserved).toBe(true);
    expect(result.imported).toBe(Number.MAX_SAFE_INTEGER - 1);
    expect(result.latest).toBe(Number.MAX_SAFE_INTEGER);
    expect(result.caughtUp).toBe(Number.MAX_SAFE_INTEGER);
    expect(result.exhausted).toBe('SerializationError');
    expect(result.overflow).toBeNull();
    expect(result.version).toBe(Number.MAX_SAFE_INTEGER);
});
