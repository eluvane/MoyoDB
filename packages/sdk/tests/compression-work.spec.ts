import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, requireCompressionStreams } from './support';

test('compression reuses owned input and preserves corruption limits', async ({ page }) => {
    await prepareMoyoDbPage(page);
    await requireCompressionStreams(page);
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
