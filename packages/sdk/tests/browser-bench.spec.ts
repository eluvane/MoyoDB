import { test, expect } from '@playwright/test';

type ProcessLike = { env?: Record<string, string | undefined> };
type BenchProfile = 'smoke' | 'standard' | 'full';
const proc = (globalThis as { process?: ProcessLike }).process;
const lifecycle = proc?.env?.npm_lifecycle_event ?? '';
const shouldRunBench = proc?.env?.MOYODB_RUN_BENCH === '1' || lifecycle.startsWith('bench:');
const engineFromLifecycle =
    lifecycle === 'bench:indexeddb'
        ? 'indexeddb'
        : lifecycle === 'bench:opfs'
          ? 'moyodb'
          : (proc?.env?.MOYODB_BENCH_ENGINE ?? 'all');
const profile = normalizeBenchProfile(proc?.env?.MOYODB_BENCH_PROFILE);
const workloadNames = normalizeWorkloadNames(proc?.env?.MOYODB_BENCH_WORKLOADS);
const sampleCountOverride = normalizeOptionalCount(proc?.env?.MOYODB_BENCH_SAMPLE_COUNT);
const warmupCountOverride = normalizeOptionalCount(proc?.env?.MOYODB_BENCH_WARMUP_COUNT);
const workloadTimeoutMs = normalizeOptionalCount(proc?.env?.MOYODB_BENCH_WORKLOAD_TIMEOUT_MS);
const testTimeoutMs = normalizeOptionalCount(proc?.env?.MOYODB_BENCH_TEST_TIMEOUT_MS);
const effectiveWorkloadTimeoutMs = workloadTimeoutMs ?? (profile === 'smoke' ? 30_000 : undefined);

test.describe('browser benchmark smoke', () => {
    test.skip(!shouldRunBench, 'benchmark smoke is opt-in; use npm run bench:browser, bench:indexeddb, or bench:opfs');

    test('runs benchmark suite and saves raw JSON', async ({ page }, testInfo) => {
        test.setTimeout(testTimeoutMs ?? (profile === 'smoke' ? 300_000 : 0));
        page.on('console', (message) => {
            if (message.type() === 'info' && message.text().startsWith('[bench]')) {
                console.log(message.text());
            }
        });
        const engines =
            engineFromLifecycle === 'all'
                ? testInfo.project.name === 'webkit'
                    ? ['moyodb']
                    : ['moyodb', 'indexeddb']
                : [engineFromLifecycle];
        await page.goto('/bench/browser-bench.html');
        const report = await page.evaluate(
            async ({
                engines,
                profile,
                workloadNames,
                sampleCountOverride,
                warmupCountOverride,
                workloadTimeoutMs,
                persistentContext
            }) => {
                if (!window.moyodbBench) {
                    throw new Error('benchmark runner did not initialize');
                }
                return await window.moyodbBench.runBenchmarkSuite({
                    profile,
                    engines: engines as Array<'moyodb' | 'indexeddb'>,
                    workloadNames,
                    sampleCountOverride,
                    warmupCountOverride,
                    workloadTimeoutMs,
                    persistentContext,
                    dbNamePrefix: `playwright-${Date.now()}`
                });
            },
            {
                engines,
                profile,
                workloadNames,
                sampleCountOverride,
                warmupCountOverride,
                workloadTimeoutMs: effectiveWorkloadTimeoutMs,
                persistentContext: false
            }
        );

        const workloadFileSegment = workloadNames ? `-${safeFileSegment(workloadNames.join('-'))}` : '';
        const resultFileName = `browser-bench-${safeFileSegment(testInfo.project.name)}-${safeFileSegment(engineFromLifecycle)}-${profile}${workloadFileSegment}.json`;
        const downloadPromise = page.waitForEvent('download');
        await page.evaluate(
            ({ report, resultFileName }) => {
                const blob = new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' });
                const url = URL.createObjectURL(blob);
                const anchor = document.createElement('a');
                anchor.href = url;
                anchor.download = resultFileName;
                document.body.append(anchor);
                anchor.click();
                anchor.remove();
                URL.revokeObjectURL(url);
            },
            { report, resultFileName }
        );
        const download = await downloadPromise;
        await download.saveAs(`bench/results/${resultFileName}`);
        await testInfo.attach('browser-bench-results', {
            body: JSON.stringify(report, null, 2),
            contentType: 'application/json'
        });

        expect(report.results.length).toBeGreaterThan(0);
        if (!report.results.some((result) => result.status === 'ok')) {
            expect(report.results.every((result) => result.status === 'skipped')).toBeTruthy();
        }
    });
});

function normalizeBenchProfile(value: string | undefined): BenchProfile {
    if (value === 'standard' || value === 'full') {
        return value;
    }
    return 'smoke';
}

function normalizeWorkloadNames(value: string | undefined): string[] | undefined {
    const names = value
        ?.split(',')
        .map((item) => item.trim())
        .filter(Boolean);
    return names && names.length > 0 ? names : undefined;
}

function normalizeOptionalCount(value: string | undefined): number | undefined {
    if (value === undefined || value.trim() === '') {
        return undefined;
    }
    const parsed = Number(value);
    return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : undefined;
}

function safeFileSegment(value: string): string {
    return (
        value
            .toLowerCase()
            .replace(/[^a-z0-9.-]+/g, '-')
            .replace(/^-|-$/g, '') || 'unknown'
    );
}
