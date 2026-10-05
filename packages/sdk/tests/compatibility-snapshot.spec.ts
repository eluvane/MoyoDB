import { readFile } from 'node:fs/promises';
import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';

for (const version of [1, 2, 3]) {
    test(`Release 1.0.1 snapshot v${version} imports, accepts writes and survives reopen`, async ({ page }) => {
        const name = uniqueDbName(`compatibility-snapshot-v${version}`);
        const bytes = await readFile(
            new URL(`../../../crates/engine/tests/fixtures/compatibility/snapshot-v${version}.bin`, import.meta.url)
        );
        await prepareMoyoDbPage(page);
        const result = await page.evaluate(
            async ({ name, bytes }) => {
                let db = await window.moyodb.openDB(name);
                try {
                    await db.importSnapshot(Uint8Array.from(bytes));
                    const schemaVersion = await db.getVersion();
                    const raw = await db.get('legacy', window.moyodb.utf8Encode('raw'));
                    const live =
                        schemaVersion === 7 && bytes[8] === 3
                            ? await db.get('docs', window.moyodb.utf8Encode('live-ttl'))
                            : null;
                    await db.put('legacy', window.moyodb.utf8Encode('new-key'), window.moyodb.utf8Encode('new-value'));
                    await db.close();
                    db = await window.moyodb.openDB(name);
                    const written = await db.get('legacy', window.moyodb.utf8Encode('new-key'));
                    const snapshot = await db.exportSnapshot();
                    const tx = await db.begin('readonly');
                    const rows = await tx.scan('legacy');
                    await tx.rollback();
                    return {
                        schemaVersion,
                        raw: raw ? window.moyodb.utf8Decode(raw) : null,
                        live: live ? window.moyodb.utf8Decode(live) : null,
                        written: written ? window.moyodb.utf8Decode(written) : null,
                        snapshotVersion: new DataView(
                            snapshot.buffer,
                            snapshot.byteOffset,
                            snapshot.byteLength
                        ).getUint32(8, true),
                        rowCount: rows.length
                    };
                } finally {
                    await db.close();
                    await window.moyodb.deleteDB(name);
                }
            },
            { name, bytes: Array.from(bytes) }
        );
        expect(result.schemaVersion).toBe(version === 1 ? 0 : 7);
        expect(result.raw).toBe('BDTTL001-raw-user-value');
        expect(result.live).toBe(version === 3 ? 'expires-in-2096' : null);
        expect(result.written).toBe('new-value');
        expect(result.snapshotVersion).toBe(3);
        expect(result.rowCount).toBe(2);
    });
}
