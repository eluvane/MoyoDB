import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { artifactManifest, hash } from './artifact-manifest.mjs';
import { chromium } from '@playwright/test';
import { createServer, version as viteVersion } from 'vite';

// Requires Node 24+. Nested times include each other; do not add them.
// Instrumentation changes elapsed time. Use the stock suite for latency measurements.
/* global window:readonly */
const kind = 'instrumented browser structural and inclusive layer measurement (separate from stock benchmark)';
const notes = [
    'worker.* and wasm.* counts are wrapped method invocations, not RPC or Worker-message counts.',
    'Layer times are inclusive and nested; do not add them together. Overlapping asynchronous methods may share wall time.',
    'Playwright response routing and instrumentation perturb elapsed time. These measurements are separate from stock browser benchmarks and do not establish cold browser or device-cache latency.',
    'Storage counts/bytes are SyncAccessHandle calls and accepted/requested byte counts, not physical disk IOPS, unique logical bytes, or a complete copy/allocation count.',
    'Preparation is captured separately; measured snapshots are taken after the original runner.run and before its original verification and cleanup.',
    'Resource entries retain the Worker lifetime; resetting metrics does not clear its performance resource buffer.',
    'Engine asset reads must match the initial path/SHA-256/byte-length manifest before diagnostic route injection; hashing adds overhead and does not establish cold startup timing.'
];
if (process.env.MEASUREMENT_KIND !== 'structural-inclusive')
    throw Error(
        'Set MEASUREMENT_KIND=structural-inclusive; these are instrumented diagnostics, not stock benchmark timings.'
    );
const baseGitSha = process.env.BENCH_GIT_SHA;
if (!baseGitSha) throw Error('BENCH_GIT_SHA is required to identify the source baseline.');
const source = path.resolve(process.env.REPO_DIR ?? fileURLToPath(new URL('../../../', import.meta.url)));
const engineDir = path.resolve(process.env.ENGINE_DIR ?? path.join(source, 'packages/sdk/public/engine'));
const persistent = process.env.PERSISTENT === '1';
const out = path.resolve(
    process.env.OUTPUT ??
        path.join(source, 'bench-results', `opfs-diagnostic-${persistent ? 'persistent' : 'incognito'}.json`)
);
await fs.mkdir(path.dirname(out), { recursive: true });
// Hash assets before browser startup to keep manifest reads outside measurements.
const artifactAssets = await artifactManifest(engineDir);
const wasmSha256 = artifactAssets['moyodb_engine_bg.wasm'].sha256;
const plugins = [
    {
        name: 'measurement-engine-artifact',
        configureServer(server) {
            const serveArtifact = async (req, res, next) => {
                const pathname = new URL(req.url, 'http://127.0.0.1').pathname;
                if (!pathname.startsWith('/engine/')) return next();
                try {
                    const relative = pathname.slice('/engine/'.length);
                    if (!Object.hasOwn(artifactAssets, relative)) throw Error(`unknown engine asset: ${relative}`);
                    const expected = artifactAssets[relative];
                    const bytes = await fs.readFile(path.join(engineDir, relative));
                    if (hash(bytes) !== expected.sha256 || bytes.length !== expected.bytes)
                        throw Error(`engine asset changed on disk: ${relative}`);
                    res.setHeader(
                        'Content-Type',
                        pathname.endsWith('.wasm') ? 'application/wasm' : 'application/javascript'
                    );
                    res.end(bytes);
                } catch (error) {
                    next(error);
                }
            };
            server.middlewares.use((req, res, next) => {
                void serveArtifact(req, res, next).catch(next);
            });
        }
    }
];
const server = await createServer({
    plugins,
    root: source + '/packages/sdk',
    configFile: source + '/packages/sdk/vite.config.ts',
    server: { host: '127.0.0.1', port: 4173, strictPort: true }
});
const launchOptions = { executablePath: process.env.CHROMIUM_EXECUTABLE, headless: true, args: ['--no-sandbox'] };
let profile = null;
let browser;
let context;
const specs = JSON.parse(
    process.env.SPECS ??
        '["large_value_64kb","large_value_1mb","small_tx_1000_commits","point_get_random_10k_pipelined","reverse_scan_limit_1","open_empty_db"]'
);
const injection = await fs.readFile(new URL('./opfs-diagnostic-worker.js', import.meta.url), 'utf8');
const diagnosticWorkerSha256 = hash(injection);
const runnerSha256 = hash(await fs.readFile(fileURLToPath(import.meta.url)));
try {
    await server.listen();
    if (persistent) {
        profile = await fs.mkdtemp(path.join(path.dirname(out), 'opfs-diagnostic-profile-'));
        context = await chromium.launchPersistentContext(profile, launchOptions);
        browser = context.browser();
        if (!browser) throw Error('persistent context did not expose its browser');
    } else {
        browser = await chromium.launch(launchOptions);
        context = await browser.newContext();
    }
    await context.route('**/src/worker.ts*', async (route) => {
        const response = await route.fetch();
        let body = await response.text();
        if (!body.includes('exposeWorkerApi(new DbWorker());')) throw new Error('worker injection anchor missing');
        body =
            injection +
            '\n' +
            body.replace(
                'exposeWorkerApi(new DbWorker());',
                `for (const key of ['open','close','begin','commit','rollback','autocommit','put','putMany','putManyPacked','get','getMany','scan','loadWasm']) globalThis.__gapWrap(DbWorker.prototype,key,'worker.'+key);
globalThis.__gapWrap(OwnershipLease.prototype,'acquire','worker.acquireLease');
const capabilitiesForMetrics={assertCapabilities};
globalThis.__gapWrap(capabilitiesForMetrics,'assertCapabilities','worker.assertCapabilities');
assertCapabilities=capabilitiesForMetrics.assertCapabilities;
exposeWorkerApi(new DbWorker());`
            );
        await route.fulfill({ response, body });
    });
    await context.route('**/engine/moyodb_engine.js', async (route) => {
        const response = await route.fetch();
        const body = await response.text();
        await route.fulfill({
            response,
            body:
                body +
                `\nif(globalThis.__gapWrap)for(const key of Object.getOwnPropertyNames(WasmEngine.prototype)){if(key!=='constructor')globalThis.__gapWrap(WasmEngine.prototype,key,'wasm.'+key);}\n`
        });
    });
    const page = await context.newPage();
    page.on('pageerror', (error) => console.error('PAGEERROR', error));
    const results = [];
    await page.goto('http://127.0.0.1:4173/bench/browser-bench.html');
    await page.waitForFunction(() => Boolean(window.moyodbBench));
    for (const spec of specs) {
        console.log('PREPARE', spec);
        await page.evaluate(async (spec) => {
            const [{ moyoDbBaseline }, { WORKLOADS }] = await Promise.all([
                import('/bench/moyodb-baseline.ts'),
                import('/bench/workloads.ts')
            ]);
            const name = typeof spec === 'string' ? spec : spec.name;
            const known = WORKLOADS.find((w) => w.name === name);
            if (!known) throw Error(`unknown workload: ${name}`);
            const workload = { ...known, ...(typeof spec === 'string' ? {} : spec) };
            const ctx = {
                dbName: `diagnostic-${name}-${Date.now()}`,
                workload,
                sampleIndex: 0,
                engine: 'moyodb',
                indexedDbDurability: 'strict'
            };
            window.__diagnostic = { runner: moyoDbBaseline, ctx };
            await moyoDbBaseline.prepare(ctx);
        }, spec);
        const beforeWorkers = page.workers().filter((w) => w.url().includes('/src/worker.ts'));
        const preparation = [];
        for (const worker of beforeWorkers)
            preparation.push(
                await worker.evaluate(() => {
                    const s = globalThis.__gapSnapshot();
                    globalThis.__gapReset();
                    return s;
                })
            );
        const elapsed = await page.evaluate(async () => {
            const { runner, ctx } = window.__diagnostic;
            const start = performance.now();
            await runner.run(ctx);
            return performance.now() - start;
        });
        const measurements = [];
        for (const worker of page.workers().filter((w) => w.url().includes('/src/worker.ts')))
            measurements.push(await worker.evaluate(() => globalThis.__gapSnapshot()));
        const checksum = await page.evaluate(async () => {
            const { runner, ctx } = window.__diagnostic;
            return runner.verify ? await runner.verify(ctx) : null;
        });
        await page.evaluate(async () => {
            const { runner, ctx } = window.__diagnostic;
            await runner.cleanup?.(ctx);
        });
        const result = { spec, elapsed, checksum, preparation, measurements };
        results.push(result);
        await fs.writeFile(
            out,
            JSON.stringify(
                {
                    kind,
                    notes,
                    measurementKind: 'structural-inclusive',
                    baseGitSha,
                    source,
                    browser: browser.version(),
                    persistentContext: persistent,
                    userDataDir: profile,
                    origin: 'http://127.0.0.1:4173',
                    sdkHarness: `Vite${viteVersion} development source`,
                    engineDir,
                    wasmSha256,
                    artifactAssets,
                    diagnosticWorkerSha256,
                    runnerSha256,
                    results
                },
                null,
                2
            ) + '\n'
        );
        console.log('RESULT', JSON.stringify({ spec, elapsed, checksum, metrics: measurements.map((m) => m.metrics) }));
    }
} finally {
    try {
        await context?.close();
    } finally {
        try {
            await browser?.close();
        } finally {
            try {
                await server.close();
            } finally {
                if (profile !== null) await fs.rm(profile, { recursive: true, force: true });
            }
        }
    }
}
