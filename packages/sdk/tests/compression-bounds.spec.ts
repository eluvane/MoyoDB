import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';

test('default Snappy stores preserve the maximum logical raw fallback through snapshot restore', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(
        async ({ sourceName, targetName }) => {
            const value = new Uint8Array(8 * 1024 * 1024);
            let state = 0x12345678;
            for (let index = 0; index < value.byteLength; index += 1) {
                state ^= state << 13;
                state ^= state >>> 17;
                state ^= state << 5;
                value[index] = state & 0xff;
            }
            const small = window.moyodb.utf8Encode('small value');
            const key = window.moyodb.utf8Encode('maximum');
            const smallKey = window.moyodb.utf8Encode('small');
            const matches = (actual: Uint8Array | null, expected: Uint8Array): boolean =>
                actual !== null &&
                actual.byteLength === expected.byteLength &&
                actual.every((byte, index) => byte === expected[index]);
            const source = await window.moyodb.openDB(sourceName);
            let snapshot: Uint8Array;
            let direct: boolean;
            try {
                await source.createStore('docs');
                await source.put('docs', key, value);
                await source.put('docs', smallKey, small);
                direct = matches(await source.get('docs', key), value);
                snapshot = await source.exportSnapshot();
            } finally {
                await source.close();
            }
            const target = await window.moyodb.openDB(targetName);
            try {
                await target.importSnapshot(snapshot);
                return {
                    direct,
                    compressionTag:
                        Number(new DataView(snapshot.buffer, snapshot.byteOffset + 60, 8).getBigUint64(0, true) >> 2n) &
                        3,
                    restored: matches(await target.get('docs', key), value),
                    small: matches(await target.get('docs', smallKey), small)
                };
            } finally {
                await target.close();
            }
        },
        { sourceName: uniqueDbName('compression-max-source'), targetName: uniqueDbName('compression-max-target') }
    );
    expect(result).toEqual({ direct: true, compressionTag: 3, restored: true, small: true });
});
