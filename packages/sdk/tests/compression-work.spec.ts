import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage } from './support';

test('compression reuses owned input and preserves corruption limits', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const supported = await page.evaluate(() => {
        return typeof CompressionStream === 'function' && typeof DecompressionStream === 'function';
    });
    test.skip(!supported, 'browser lacks CompressionStream/DecompressionStream');
    const result = await page.evaluate(async () => {
        const modulePath = '/src/compression.ts';
        const casesPath = '/tests/compression-work-cases.ts';
        const api = (await import(modulePath)) as typeof import('../src/compression');
        const { checkCompressionWork } = (await import(casesPath)) as typeof import('./compression-work-cases');
        return checkCompressionWork(api);
    });
    expect(result.cases).toBe(74);
    expect(result.blobConstructions).toBe(0);
});
