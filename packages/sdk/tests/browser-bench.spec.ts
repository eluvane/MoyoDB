import { test as base, expect, type BrowserContext } from '@playwright/test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

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
const sampleCountOverride = normalizeOptionalCount(proc?.env?.MOYODB_BENCH_SAMPLE_COUNT, 'MOYODB_BENCH_SAMPLE_COUNT');
const warmupCountOverride = normalizeOptionalCount(proc?.env?.MOYODB_BENCH_WARMUP_COUNT, 'MOYODB_BENCH_WARMUP_COUNT');
const workloadTimeoutMs = normalizeOptionalCount(
    proc?.env?.MOYODB_BENCH_WORKLOAD_TIMEOUT_MS,
    'MOYODB_BENCH_WORKLOAD_TIMEOUT_MS'
);
const testTimeoutMs = normalizeOptionalCount(proc?.env?.MOYODB_BENCH_TEST_TIMEOUT_MS, 'MOYODB_BENCH_TEST_TIMEOUT_MS');
if (sampleCountOverride === 0) {
    throw new Error('MOYODB_BENCH_SAMPLE_COUNT must be a positive integer');
}
const effectiveWorkloadTimeoutMs = workloadTimeoutMs ?? (profile === 'smoke' ? 30_000 : undefined);
const gitSha = proc?.env?.MOYODB_BENCH_GIT_SHA ?? proc?.env?.GITHUB_SHA ?? 'unknown';
const indexedDbDurability = normalizeDurability(proc?.env?.MOYODB_BENCH_IDB_DURABILITY);
const persistentContext = proc?.env?.MOYODB_BENCH_PERSISTENT_CONTEXT === '1';

// Use the Playwright fixture to avoid launching an unused default browser.
const test = persistentContext
    ? base.extend({
          context: async ({ playwright, browserName, launchOptions, headless, channel, baseURL }, use) => {
              const userDataDir = await mkdtemp(join(tmpdir(), 'moyodb-bench-profile-'));
              let context: BrowserContext | undefined;
              try {
                  // The Playwright fixture supplies project context options here.
                  context = await playwright[browserName].launchPersistentContext(userDataDir, {
                      ...launchOptions,
                      headless,
                      channel,
                      baseURL
                  });
                  await use(context);
              } finally {
                  try {
                      await context?.close();
                  } finally {
                      await rm(userDataDir, { recursive: true, force: true });
                  }
              }
          }
      })
    : base;

test.describe('browser benchmark smoke', () => {
    test.skip(!shouldRunBench, 'benchmark smoke is opt-in; use npm run bench:browser, bench:indexeddb, or bench:opfs');

    test('runs benchmark suite and saves raw JSON', async ({ page }, testInfo) => {
        test.setTimeout(testTimeoutMs ?? (profile === 'smoke' ? 300_000 : 0));
        page.on('console', (message) => {
            if (message.type() === 'info' && message.text().startsWith('[bench]')) {
                console.info(message.text());
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
                persistentContext,
                gitSha,
                indexedDbDurability
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
                    gitSha,
                    indexedDbDurability,
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
                persistentContext,
                gitSha,
                indexedDbDurability
            }
        );

        const workloadFileSegment = workloadNames ? `-${safeFileSegment(workloadNames.join('-'))}` : '';
        const contextFileSegment = persistentContext ? '-persistent' : '';
        const resultFileName = `browser-bench-${safeFileSegment(testInfo.project.name)}-${safeFileSegment(engineFromLifecycle)}-${profile}${contextFileSegment}${workloadFileSegment}.json`;
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
        expect(report.environment.persistentContext).toBe(persistentContext);
        expect(
            report.results
                .filter((result) => result.status === 'error')
                .map(({ engine, workloadName, error }) => ({ engine, workloadName, error })),
            'Every applicable benchmark workload must complete successfully.'
        ).toEqual([]);
        if (!report.results.some((result) => result.status === 'ok')) {
            expect(report.results.every((result) => result.status === 'skipped')).toBeTruthy();
        }
    });
});

function normalizeBenchProfile(value: string | undefined): BenchProfile {
    if (value === undefined || value.trim() === '' || value === 'smoke') {
        return 'smoke';
    }
    if (value === 'standard' || value === 'full') {
        return value;
    }
    throw new Error('MOYODB_BENCH_PROFILE must be smoke, standard, or full');
}

function normalizeDurability(value: string | undefined): IDBTransactionDurability {
    if (value === undefined || value.trim() === '' || value === 'strict') {
        return 'strict';
    }
    if (value === 'relaxed' || value === 'default') {
        return value;
    }
    throw new Error('MOYODB_BENCH_IDB_DURABILITY must be strict, relaxed, or default');
}

function normalizeWorkloadNames(value: string | undefined): string[] | undefined {
    const names = value
        ?.split(',')
        .map((item) => item.trim())
        .filter(Boolean);
    return names && names.length > 0 ? names : undefined;
}

function normalizeOptionalCount(value: string | undefined, name: string): number | undefined {
    if (value === undefined || value.trim() === '') {
        return undefined;
    }
    const parsed = Number(value);
    if (!Number.isSafeInteger(parsed) || parsed < 0) {
        throw new Error(`${name} must be a non-negative integer`);
    }
    return parsed;
}

function safeFileSegment(value: string): string {
    return (
        value
            .toLowerCase()
            .replace(/[^a-z0-9.-]+/g, '-')
            .replace(/^-|-$/g, '') || 'unknown'
    );
}
