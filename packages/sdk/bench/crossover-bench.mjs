import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { artifactManifest, hash } from './artifact-manifest.mjs';
import { chromium } from '@playwright/test';
import { createServer, version as viteVersion } from 'vite';

/* global window:readonly */

// Run with Node 24+ from the repository root after npm ci and both WASM builds.
// BASELINE_ENGINE_DIR points at the original release engine directory.
// AFTER_ENGINE_DIR defaults to this checkout's generated release engine.
const root = path.resolve(process.env.REPO_DIR ?? fileURLToPath(new URL('../../../', import.meta.url)));
const measurement = path.resolve(process.env.OUTPUT_DIR ?? path.join(root, 'bench-results/crossover'));
const source = path.resolve(process.env.BENCH_SOURCE_DIR ?? root);
await fs.mkdir(measurement, { recursive: true });
const { computeStats } = await import(pathToFileURL(path.join(source, 'packages/sdk/bench/report.ts')).href);
const mode = process.env.MODE ?? 'persistent';
if (!['persistent', 'incognito'].includes(mode)) throw Error('MODE must be persistent or incognito');
const persistent = mode === 'persistent';
const baseGitSha = process.env.BENCH_GIT_SHA ?? 'unknown';
if (!process.env.BASELINE_ENGINE_DIR) throw Error('BASELINE_ENGINE_DIR is required');
const workloadNames = [
    'large_value_64kb',
    'large_value_1mb',
    'small_tx_1000_commits',
    'point_get_random_10k_pipelined',
    'reverse_scan_limit_1'
];
const artifacts = [
    { name: 'baseline', engineDir: path.resolve(process.env.BASELINE_ENGINE_DIR) },
    {
        name: 'combined',
        engineDir: path.resolve(process.env.AFTER_ENGINE_DIR ?? path.join(root, 'packages/sdk/public/engine'))
    }
];
for (const artifact of artifacts) {
    artifact.assets = await artifactManifest(artifact.engineDir);
    artifact.wasmSha256 = artifact.assets['moyodb_engine_bg.wasm'].sha256;
}
if (artifacts[0].wasmSha256 === artifacts[1].wasmSha256) throw Error('before and after WASM bytes are identical');
let activeArtifact = artifacts[0];
let activeSuite = 'initial-page';
let pendingAssetReads = 0;
const assetResponses = [];
const reports = { baseline: [], combined: [] };
const plugins = [
    {
        name: 'verified-single-profile-engine-crossover',
        configureServer(server) {
            const serveArtifact = async (request, response, next) => {
                const pathname = new URL(request.url, 'http://127.0.0.1').pathname;
                if (!pathname.startsWith('/engine/')) return next();
                const artifact = activeArtifact;
                const suite = activeSuite;
                pendingAssetReads++;
                try {
                    const relative = pathname.slice('/engine/'.length);
                    if (!Object.hasOwn(artifact.assets, relative)) throw Error(`unknown engine asset: ${relative}`);
                    const expected = artifact.assets[relative];
                    const bytes = await fs.readFile(path.join(artifact.engineDir, relative));
                    const sha256 = hash(bytes);
                    const isWasm = pathname.endsWith('.wasm');
                    if (sha256 !== expected.sha256 || bytes.length !== expected.bytes)
                        throw Error(`engine asset changed on disk: ${relative}`);
                    response.setHeader('Content-Type', isWasm ? 'application/wasm' : 'application/javascript');
                    response.setHeader('Cache-Control', 'no-store');
                    response.setHeader('Pragma', 'no-cache');
                    response.setHeader('X-Measurement-Artifact', artifact.name);
                    response.setHeader('X-Measurement-Sha256', sha256);
                    assetResponses.push({
                        suite,
                        artifact: artifact.name,
                        pathname,
                        sha256,
                        bytes: bytes.length,
                        expectedSha256: expected.sha256,
                        cacheControl: 'no-store'
                    });
                    response.end(bytes);
                } catch (error) {
                    next(error);
                } finally {
                    pendingAssetReads--;
                }
            };
            server.middlewares.use((request, response, next) => {
                void serveArtifact(request, response, next).catch(next);
            });
        }
    }
];
const server = await createServer({
    plugins,
    cacheDir: path.join(measurement, `vite-crossover-cache-${mode}`),
    root: path.join(source, 'packages/sdk'),
    configFile: path.join(source, 'packages/sdk/vite.config.ts'),
    server: { host: '127.0.0.1', port: 4173, strictPort: true }
});
const launchOptions = {
    executablePath: process.env.CHROMIUM_EXECUTABLE || undefined,
    headless: true,
    args: ['--no-sandbox']
};
const prefix = path.join(measurement, `crossover-five-${mode}`);
let profile = null;
let browser;
let context;
let page;
let pageCreationIndex = 0;
try {
    await server.listen();
    if (persistent) {
        profile = await fs.mkdtemp(path.join(measurement, 'crossover-profile-'));
        context = await chromium.launchPersistentContext(profile, launchOptions);
        browser = context.browser();
        if (!browser) throw Error('persistent context did not expose its browser');
    } else {
        browser = await chromium.launch(launchOptions);
        context = await browser.newContext();
    }
    // A persistent context may include an initial about:blank page.
    for (const initialPage of context.pages()) await initialPage.close();
    for (let round = 0; round < 6; round++) {
        const order = round % 2 === 0 ? artifacts : artifacts.toReversed();
        for (const artifact of order) {
            // Close the page before switching artifacts. Its environment probe
            // caches the initialized WASM module, so each artifact needs a fresh page.
            if (page) {
                await page.close();
                page = undefined;
            }
            if (context.pages().length !== 0) throw Error('previous suite page is still open');
            if (pendingAssetReads !== 0) throw Error('cannot switch artifact while asset reads are pending');
            activeArtifact = artifact;
            activeSuite = `${mode}-${round}-${artifact.name}`;
            const responseStart = assetResponses.length;
            page = await context.newPage();
            pageCreationIndex++;
            if (context.pages().length !== 1) throw Error('suite must own exactly one fresh page');
            let dbWorkerLoads = 0;
            page.on('worker', (worker) => {
                if (new URL(worker.url()).pathname === '/src/worker.ts') dbWorkerLoads++;
            });
            page.on('console', (message) => {
                if (message.text().startsWith('[bench]')) console.log(activeArtifact.name, message.text());
            });
            page.on('pageerror', (error) => console.error('PAGEERROR', error));
            await page.goto('http://127.0.0.1:4173/bench/browser-bench.html');
            await page.waitForFunction(() => Boolean(window.moyodbBench));
            await page.evaluate(async () => {
                const [{ moyoDbBaseline }, { indexedDbBaseline }] = await Promise.all([
                    import('/bench/moyodb-baseline.ts'),
                    import('/bench/indexeddb-baseline.ts')
                ]);
                window.__sampleIndexOffset = 0;
                for (const runner of [moyoDbBaseline, indexedDbBaseline]) {
                    const prepare = runner.prepare;
                    runner.prepare = function (ctx) {
                        ctx.sampleIndex += window.__sampleIndexOffset;
                        return prepare.call(this, ctx);
                    };
                }
            });
            console.log(
                'ROUND_START',
                JSON.stringify({
                    mode,
                    round,
                    artifact: artifact.name,
                    sha: artifact.wasmSha256,
                    time: new Date().toISOString()
                })
            );
            const report = await page.evaluate(
                async ({ workloadNames, round, persistent, variant, baseGitSha }) => {
                    window.__sampleIndexOffset = round;
                    return window.moyodbBench.runBenchmarkSuite({
                        profile: 'full',
                        engines: ['moyodb', 'indexeddb'],
                        workloadNames,
                        sampleCountOverride: 1,
                        warmupCountOverride: round === 0 ? 1 : 0,
                        workloadTimeoutMs: 300000,
                        persistentContext: persistent,
                        gitSha: baseGitSha + (variant === 'baseline' ? '' : '-dirty'),
                        indexedDbDurability: 'strict',
                        dbNamePrefix: `crossover-${variant}-${round}-${Date.now()}`
                    });
                },
                { workloadNames, round, persistent, variant: artifact.name, baseGitSha }
            );
            if (pendingAssetReads !== 0) throw Error('stock suite returned with pending engine asset reads');
            // Check the instance cached by the environment probe without fetching
            // engine assets again.
            const responsesBeforeMainCheck = assetResponses.length;
            const mainPageBuildProfile = await page.evaluate(async () => {
                const engine = await import(new URL('/engine/moyodb_engine.js', window.location.href).href);
                return engine.buildProfile();
            });
            if (mainPageBuildProfile !== 'release' || report.environment.wasmBuildMode !== 'release') {
                throw Error(`main-page engine is not initialized release WASM in ${activeSuite}`);
            }
            if (assetResponses.length !== responsesBeforeMainCheck || pendingAssetReads !== 0) {
                throw Error('main-page module check unexpectedly fetched engine assets');
            }
            const suiteResponses = assetResponses.slice(responseStart);
            const wasmResponses = suiteResponses.filter((response) => response.pathname.endsWith('.wasm'));
            const glueResponses = suiteResponses.filter((response) => response.pathname === '/engine/moyodb_engine.js');
            const snippetPaths = Object.keys(artifact.assets).filter(
                (relative) => relative.startsWith('snippets/') && relative.endsWith('.js')
            );
            const snippetResponses = suiteResponses.filter((response) =>
                response.pathname.startsWith('/engine/snippets/')
            );
            const expectedWorkerLoads = workloadNames.length * (round === 0 ? 2 : 1);
            const expectedLoads = expectedWorkerLoads + 1;
            const snippetLoadsByPath = Object.fromEntries(
                snippetPaths.map((relative) => [
                    relative,
                    snippetResponses.filter((response) => response.pathname === `/engine/${relative}`).length
                ])
            );
            if (
                dbWorkerLoads !== expectedWorkerLoads ||
                wasmResponses.length !== expectedLoads ||
                glueResponses.length !== expectedLoads ||
                snippetPaths.length === 0 ||
                snippetResponses.length !== expectedLoads * snippetPaths.length ||
                Object.values(snippetLoadsByPath).some((loads) => loads !== expectedLoads)
            ) {
                throw Error(
                    `unexpected fresh engine fetches ${activeSuite}: workers=${dbWorkerLoads}, wasm=${wasmResponses.length}, glue=${glueResponses.length}, snippets=${JSON.stringify(snippetLoadsByPath)}, expected workers=${expectedWorkerLoads} plus one main-page probe`
                );
            }
            // The environment probe runs before workloads, so the first responses
            // belong to the main page.
            const mainPageProbe = {
                wasm: wasmResponses[0],
                glue: glueResponses[0],
                snippets: snippetPaths.map((relative) =>
                    snippetResponses.find((response) => response.pathname === `/engine/${relative}`)
                ),
                buildProfile: mainPageBuildProfile
            };
            if (
                suiteResponses.some(
                    (response) =>
                        response.suite !== activeSuite ||
                        response.artifact !== artifact.name ||
                        response.cacheControl !== 'no-store'
                )
            ) {
                throw Error(`mixed artifact response in ${activeSuite}`);
            }
            if (wasmResponses.some((response) => response.sha256 !== artifact.wasmSha256)) {
                throw Error(`wrong WASM bytes served in ${activeSuite}`);
            }
            if (suiteResponses.some((response) => response.sha256 !== response.expectedSha256)) {
                throw Error(`wrong engine JS/WASM/snippet bytes served in ${activeSuite}`);
            }
            report.environment.moyoDbDurability =
                'strict: WAL flushed before commit resolves; main and manifest flushed during checkpoint';
            report.notes = report.notes.map((note) =>
                note.startsWith('Durability:')
                    ? 'Durability: IndexedDB explicit readwrite transactions use durability "strict"; MoyoDB flushes WAL before commit resolves and flushes main/manifest during checkpoint.'
                    : note
            );
            report.execution = {
                designVersion: 2,
                design: "One browser/profile/context/origin shared by both artifacts, with a fresh page for each artifact suite. The previous page closes after stock cleanup and before artifact selection changes. Each page's unchanged environment probe retains only its selected artifact for that suite, symmetrically; no page-level module survives into the next suite. Identical Cache-Control:no-store on both artifacts; all served JS/WASM/snippet assets checked against their manifest. Empty-open excluded because artifact loading is controlled rather than startup cache semantics.",
                round,
                variant: artifact.name,
                mode,
                wasmSha256: artifact.wasmSha256,
                engineDir: artifact.engineDir,
                source,
                artifactAssets: artifact.assets,
                userDataDir: profile,
                browserVersion: browser.version(),
                sampleIndexOffset: round,
                pageLifetime: 'fresh-per-artifact-suite',
                pageCreationIndex,
                fetchVerification: {
                    expectedWorkerLoads,
                    dbWorkerLoads,
                    expectedLoads,
                    mainPageProbe,
                    wasmLoads: wasmResponses.length,
                    glueLoads: glueResponses.length,
                    snippetLoadsByPath,
                    allWasmHashesMatch: true,
                    allAssetHashesMatch: true,
                    responses: suiteResponses
                }
            };
            reports[artifact.name].push(report);
            await fs.writeFile(`${prefix}-${artifact.name}-round${round}.json`, JSON.stringify(report, null, 2) + '\n');
            await fs.writeFile(`${prefix}-asset-responses.json`, JSON.stringify(assetResponses, null, 2) + '\n');
            if (
                report.results.length !== workloadNames.length * 2 ||
                report.results.some((result) => result.status !== 'ok')
            ) {
                throw Error(`failed or missing crossover workload ${activeSuite}`);
            }
            console.log(
                'ROUND_DONE',
                JSON.stringify({
                    mode,
                    round,
                    artifact: artifact.name,
                    wasmLoads: wasmResponses.length,
                    sha: artifact.wasmSha256,
                    results: report.results.map((result) => ({
                        engine: result.engine,
                        workload: result.workloadName,
                        ms: result.rawSamples[0]
                    }))
                })
            );
        }
    }
    if (page) {
        await page.close();
        page = undefined;
    }
    if (pendingAssetReads !== 0) throw Error('final page closed with pending engine asset reads');
    for (const artifact of artifacts) {
        const rounds = reports[artifact.name];
        const merged = {
            ...rounds[0],
            generatedAt: new Date().toISOString(),
            results: rounds[0].results.map((first) => {
                const all = rounds.flatMap((report) =>
                    report.results.filter(
                        (row) => row.engine === first.engine && row.workloadName === first.workloadName
                    )
                );
                const rawSamples = all.flatMap((row) => row.rawSamples);
                return {
                    ...first,
                    sampleCount: rawSamples.length,
                    warmupCount: all.reduce((count, row) => count + row.warmupSamples.length, 0),
                    rawSamples,
                    warmupSamples: all.flatMap((row) => row.warmupSamples),
                    contentChecksums: all.flatMap((row) => row.contentChecksums),
                    stats: computeStats(rawSamples)
                };
            }),
            execution: {
                ...rounds[0].execution,
                pairedRounds: 6,
                order: 'AB,BA,AB,BA,AB,BA; one initial warmup and six measured samples per artifact/engine/workload; sample seed indices0..5; one shared profile/context/origin and fresh page for every artifact suite',
                pageCreationIndices: rounds.map((report) => report.execution.pageCreationIndex),
                sdkHarness: `Vite${viteVersion} unbundled TypeScript; production release WASM`,
                fetchVerification: {
                    allRoundsVerified: true,
                    allAssetHashesMatch: true,
                    allMainPageProbesVerified: true,
                    expectedLoads: rounds.map((report) => report.execution.fetchVerification.expectedLoads),
                    dbWorkerLoads: rounds.map((report) => report.execution.fetchVerification.dbWorkerLoads),
                    mainPageProbes: rounds.map((report) => report.execution.fetchVerification.mainPageProbe),
                    wasmLoads: rounds.map((report) => report.execution.fetchVerification.wasmLoads),
                    glueLoads: rounds.map((report) => report.execution.fetchVerification.glueLoads),
                    snippetLoadsByPath: rounds.map((report) => report.execution.fetchVerification.snippetLoadsByPath),
                    expectedWasmSha256: artifact.wasmSha256,
                    responseLog: `${prefix}-asset-responses.json`
                }
            }
        };
        await fs.writeFile(`${prefix}-${artifact.name}.json`, JSON.stringify(merged, null, 2) + '\n');
        console.log(
            'COMPLETE',
            JSON.stringify({
                mode,
                artifact: artifact.name,
                results: merged.results.map((result) => ({
                    engine: result.engine,
                    workload: result.workloadName,
                    samples: result.rawSamples,
                    checksums: result.contentChecksums
                }))
            })
        );
    }
} finally {
    try {
        try {
            await context?.close();
        } finally {
            await browser?.close();
        }
    } finally {
        try {
            await server.close();
        } finally {
            if (profile !== null) await fs.rm(profile, { recursive: true, force: true });
        }
    }
}
