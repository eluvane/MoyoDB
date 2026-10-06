import { expect, test } from '@playwright/test';
import type * as Codec from '../src/codec';
import type * as Indexing from '../src/indexing';
import type * as Compression from '../src/compression';
import type * as Cases from './codec-index-work-cases';
import { prepareMoyoDbPage } from './support';

test('key, JSON, index and compression codecs preserve boundary contracts', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const supported = await page.evaluate(() => {
        return typeof CompressionStream === 'function' && typeof DecompressionStream === 'function';
    });
    test.skip(!supported, 'browser lacks CompressionStream/DecompressionStream');
    const result = await page.evaluate(async () => {
        const codecPath = '/src/codec.ts';
        const indexingPath = '/src/indexing.ts';
        const compressionPath = '/src/compression.ts';
        const casesPath = '/tests/codec-index-work-cases.ts';
        const [codec, indexing, compression, cases] = await Promise.all([
            import(codecPath) as Promise<typeof Codec>,
            import(indexingPath) as Promise<typeof Indexing>,
            import(compressionPath) as Promise<typeof Compression>,
            import(casesPath) as Promise<typeof Cases>
        ]);
        return cases.checkCodecIndexWork({
            codec,
            indexing,
            compression
        });
    });
    expect(result.results.filter((entry) => !entry.passed)).toEqual([]);
    expect(result.passed).toBe(23);
});
