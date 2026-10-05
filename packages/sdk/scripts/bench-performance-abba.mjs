import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';

const defaultWorkloads = [
    'open_empty_db',
    'point_get_random_10k_pipelined',
    'reverse_scan_limit_1',
    'small_tx_1000_commits',
    'large_value_64kb',
    'large_value_1mb'
];
const { values } = parseArgs({
    options: {
        before: { type: 'string' },
        after: { type: 'string', default: process.cwd() },
        'dependency-root': { type: 'string', default: process.cwd() },
        out: { type: 'string' },
        profiles: { type: 'string', default: '3' },
        samples: { type: 'string', default: '1' },
        warmups: { type: 'string', default: '1' },
        workloads: { type: 'string', default: defaultWorkloads.join(',') },
        'value-sizes': { type: 'string' },
        records: { type: 'string', default: '2048' },
        'batch-size': { type: 'string', default: '256' },
        'total-bytes': { type: 'string' },
        'batch-bytes': { type: 'string', default: '8388608' },
        'base-port': { type: 'string', default: '4373' },
        'timeout-ms': { type: 'string', default: '300000' },
        help: { type: 'boolean', default: false }
    }
});
if (values.help) {
    console.log(`Usage: node packages/sdk/scripts/bench-performance-abba.mjs --before BASELINE --after CANDIDATE --out RAW.json
  Build release WASM independently in both immutable source trees first.
  --dependency-root REPO_WITH_INSTALLED_PLAYWRIGHT_AND_VITE
  --profiles 3 --samples 1 --warmups 1
  --workloads ${defaultWorkloads.join(',')}
  --value-sizes 1008,1024,2048,4033,4050,4066,8192,16384 --records 2048 --batch-size 256
                                    Run only symmetric fixed-record scaling rows
  --value-sizes 4096,16384,65536,262144,1048576 --total-bytes 67108864 --batch-bytes 8388608
                                    Run only scaling rows with fixed payload/transaction bytes
  --base-port 4373 --timeout-ms 300000
  MOYODB_CHROMIUM_EXECUTABLE_PATH selects an installed Chromium.
Each profile warms both sources, then runs ABBA (BAAB on alternate profiles).
Each visit collects --samples samples per workload/engine. Defaults yield six
measured samples per variant/workload and twelve strict IndexedDB controls.
The sources use separate fixed origins; origin assignments swap each profile.
Stock timed run/verify functions are unchanged. An untimed prepare wrapper
advances the official workload's random seed equally for both engines.`);
    process.exit(0);
}
assert.ok(values.before, '--before is required');
function count(option, minimum) {
    const n = Number(values[option]);
    assert.ok(Number.isSafeInteger(n) && n >= minimum, `invalid --${option}`);
    return n;
}
const profileCount = count('profiles', 1);
const samples = count('samples', 1);
const warmups = count('warmups', 0);
const timeoutMs = count('timeout-ms', 1);
const basePort = count('base-port', 1024);
assert.ok(basePort < 65535, '--base-port must leave room for a second port');
let workloadNames = values.workloads
    .split(',')
    .map((name) => name.trim())
    .filter(Boolean);
const scalingSpecs = [];
if (values['value-sizes']) {
    const totalBytes = values['total-bytes'] ? count('total-bytes', 1) : null;
    const batchBytes = totalBytes === null ? null : count('batch-bytes', 1);
    if (totalBytes !== null) assert.equal(totalBytes % batchBytes, 0);
    const sizes = values['value-sizes'].split(',').map(Number);
    assert.equal(new Set(sizes).size, sizes.length, 'duplicate value sizes');
    for (const valueSize of sizes) {
        assert.ok(Number.isSafeInteger(valueSize) && valueSize > 0, 'invalid value size');
        if (batchBytes !== null) assert.equal(batchBytes % valueSize, 0, 'value size must divide transaction bytes');
        const recordCount = totalBytes === null ? count('records', 1) : totalBytes / valueSize;
        const batchSize = batchBytes === null ? count('batch-size', 1) : batchBytes / valueSize;
        assert.ok(batchSize <= recordCount);
        scalingSpecs.push({
            name: `large_value_scaling_${valueSize}b`,
            recordCount,
            batchSize,
            valueSize,
            transactionBoundaries: `${Math.ceil(recordCount / batchSize)} readwrite transactions; batches of at most ${batchSize} values; ${recordCount * valueSize} total value bytes`,
            notes: `Additional symmetric ${totalBytes === null ? 'fixed-record' : 'fixed-byte'} scaling row using unchanged stock runners and deterministic input. Setup/generation/cleanup excluded for both engines; stock write verification samples up to 1024 records.`
        });
    }
    workloadNames = scalingSpecs.map((spec) => spec.name);
}
assert.ok(workloadNames.length > 0 && new Set(workloadNames).size === workloadNames.length);
const roots = { before: resolve(values.before), after: resolve(values.after) };
assert.notEqual(roots.before, roots.after, 'Use two immutable source trees');
const timestamp = new Date().toISOString();
const out = resolve(
    values.out ?? join(roots.after, 'packages/sdk/bench/results', `abba-${timestamp.replaceAll(':', '-')}.json`)
);
await mkdir(dirname(out), { recursive: true });
const require = createRequire(join(resolve(values['dependency-root']), 'package.json'));
const { chromium } = require('@playwright/test');
const { createServer } = await import(pathToFileURL(require.resolve('vite')).href);
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
function git(root, ...args) {
    return execFileSync('git', args, {
        cwd: root,
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore']
    }).trim();
}
async function fingerprint(root) {
    const paths = git(root, 'ls-files', '--cached', '--others', '--exclude-standard')
        .split('\n')
        .filter((path) =>
            /^(?:Cargo\.(?:toml|lock)|rust-toolchain\.toml|crates\/engine\/(?:Cargo\.toml|src\/.*\.rs|js\/.*\.js)|packages\/sdk\/(?:src\/.*\.ts|bench\/.*\.ts|vite\.config\.ts))$/.test(
                path
            )
        );
    async function runtimeAssets(prefix = '') {
        const assets = [];
        for (const entry of await readdir(join(root, 'packages/sdk/public/engine', prefix), { withFileTypes: true })) {
            const path = prefix ? `${prefix}/${entry.name}` : entry.name;
            if (entry.isDirectory()) assets.push(...(await runtimeAssets(path)));
            else if (/\.(?:js|mjs|wasm)$/.test(entry.name)) assets.push(`packages/sdk/public/engine/${path}`);
        }
        return assets;
    }
    paths.push(...(await runtimeAssets()).sort());
    const files = {};
    for (const path of paths) {
        const bytes = await readFile(join(root, path));
        files[path] = { sha256: hash(bytes), bytes: bytes.length };
    }
    assert.ok(files['packages/sdk/public/engine/moyodb_engine.js']);
    assert.ok(files['packages/sdk/public/engine/moyodb_engine_bg.wasm']);
    const shims = Object.keys(files).filter((path) => /\/snippets\/[^/]+\/js\/opfs_shim\.js$/.test(path));
    assert.equal(shims.length, 1, 'Exactly one generated OPFS shim expected');
    assert.deepEqual(
        files[shims[0]],
        files['crates/engine/js/opfs_shim.js'],
        'Generated OPFS shim differs from source'
    );
    return {
        root,
        head: git(root, 'rev-parse', 'HEAD'),
        trackedDiff: git(root, 'diff', '--stat', 'HEAD'),
        files
    };
}
async function hostSnapshot() {
    const snapshot = { timestamp: new Date().toISOString() };
    for (const [key, path] of [
        ['load', '/proc/loadavg'],
        ['cpu', '/sys/fs/cgroup/cpu.stat'],
        ['memoryBytes', '/sys/fs/cgroup/memory.current']
    ]) {
        snapshot[key] = await readFile(path, 'utf8').catch(() => null);
    }
    try {
        snapshot.processes = execFileSync('ps', ['-eo', 'pid,ppid,pcpu,comm', '--sort=-pcpu'], {
            encoding: 'utf8',
            stdio: ['ignore', 'pipe', 'ignore']
        })
            .split('\n')
            .slice(0, 18);
    } catch {
        snapshot.processes = null;
    }
    return snapshot;
}
function parity(report) {
    for (const name of workloadNames) {
        const rows = report.results.filter((row) => row.workloadName === name);
        assert.equal(rows.length, 2, `${name}: both engine results required`);
        for (const row of rows) assert.equal(row.status, 'ok', `${name}/${row.engine}: ${row.error ?? row.notes}`);
        for (const field of [
            'recordCount',
            'keySize',
            'valueSize',
            'batchSize',
            'transactionBoundaries',
            'sampleCount',
            'warmupCount'
        ]) {
            assert.equal(rows[0][field], rows[1][field], `${name}: ${field} parity`);
        }
        assert.deepEqual(rows[0].contentChecksums, rows[1].contentChecksums, `${name}: content parity`);
    }
    assert.equal(report.environment.wasmBuildMode, 'release', 'Release WASM required');
    assert.equal(report.environment.indexedDbDurability, 'strict');
}
async function verifyAssets(context, url, expected) {
    const assets = {};
    const prefix = 'packages/sdk/public/engine/';
    for (const path of Object.keys(expected.files).filter((path) => path.startsWith(prefix))) {
        const name = path.slice(prefix.length);
        const response = await context.request.get(`${url}/engine/${name}`);
        assert.ok(response.ok(), `HTTP artifact fetch failed: ${response.status()}`);
        const bytes = await response.body();
        const record = { sha256: hash(bytes), bytes: bytes.length };
        assert.deepEqual(record, expected.files[path], `served ${name} differs from source artifact`);
        assets[name] = record;
    }
    return assets;
}
const initial = {
    before: await fingerprint(roots.before),
    after: await fingerprint(roots.after)
};
for (const path of Object.keys(initial.before.files).filter(
    (path) => path.startsWith('packages/sdk/bench/') && path.endsWith('.ts')
)) {
    assert.deepEqual(initial.after.files[path], initial.before.files[path], `Stock benchmark source differs: ${path}`);
}
const output = {
    kind: 'counterbalanced full browser comparison',
    timestamp,
    runnerSha256: hash(await readFile(fileURLToPath(import.meta.url))),
    runtime: {
        node: process.version,
        platform: `${process.platform}/${process.arch}`,
        playwright: require('@playwright/test/package.json').version,
        vite: require('vite/package.json').version
    },
    configuration: {
        roots,
        profileCount,
        samplesPerVisit: samples,
        warmupsPerVariant: warmups,
        workloadNames,
        scalingSpecs,
        basePort,
        timeoutMs
    },
    sources: initial,
    qualifications: [
        'Each launch uses a fresh persistent Chromium profile; sources have immutable separate origins whose assignments alternate between launches.',
        'Stock workload run and verify functions, inputs, transaction boundaries, strict durability options and timed regions are unchanged.',
        'An untimed prepare wrapper assigns successive stock random seeds to corresponding samples in both engines and both variants.',
        'HTTP artifact bytes are verified before and after each launch, outside every timed sample; no cache-control or request interception is installed.',
        'Fresh empty database open is not cold browser/network/module startup; warmups and stock release-profile detection precede measured samples.',
        'Headless persistent browser storage and fsync costs are specific to this host; no hardware power-loss guarantee is inferred from timings.'
    ],
    launches: []
};
const save = () => writeFile(out, JSON.stringify(output, null, 2) + '\n');
try {
    for (let launch = 0; launch < profileCount; launch += 1) {
        const profile = await mkdtemp(join(tmpdir(), 'moyo-abba-'));
        const cacheRoot = await mkdtemp(join(tmpdir(), 'moyo-abba-vite-'));
        const servers = [],
            pages = {};
        let context;
        const order =
            launch % 2 === 0 ? ['before', 'after', 'after', 'before'] : ['after', 'before', 'before', 'after'];
        const record = {
            launch,
            order,
            warmups: [],
            visits: [],
            browserErrors: [],
            hostBefore: await hostSnapshot()
        };
        output.launches.push(record);
        try {
            const urls = {};
            for (const [index, variant] of ['before', 'after'].entries()) {
                const port = basePort + ((index + launch) % 2);
                urls[variant] = `http://127.0.0.1:${port}`;
                const server = await createServer({
                    root: join(roots[variant], 'packages/sdk'),
                    configFile: join(roots[variant], 'packages/sdk/vite.config.ts'),
                    cacheDir: join(cacheRoot, variant),
                    server: { host: '127.0.0.1', port, strictPort: true }
                });
                servers.push(server);
                await server.listen();
            }
            context = await chromium.launchPersistentContext(profile, {
                executablePath:
                    process.env.MOYODB_CHROMIUM_EXECUTABLE_PATH || process.env.MOYO_CHROMIUM_EXECUTABLE || undefined,
                headless: true,
                args: ['--enable-automation']
            });
            record.urls = urls;
            record.httpAssetsBefore = {};
            for (const variant of ['before', 'after']) {
                record.httpAssetsBefore[variant] = await verifyAssets(context, urls[variant], initial[variant]);
                const page = await context.newPage();
                pages[variant] = page;
                page.on('pageerror', (error) => record.browserErrors.push({ variant, error: String(error) }));
                await page.goto(`${urls[variant]}/bench/browser-bench.html`, {
                    waitUntil: 'networkidle'
                });
                await page.waitForFunction(() => typeof globalThis.moyodbBench?.runBenchmarkSuite === 'function');
                await page.evaluate(
                    async ({ scalingSpecs, workloadNames }) => {
                        const [{ moyoDbBaseline }, { indexedDbBaseline }, { WORKLOADS }] = await Promise.all([
                            import(new URL('/bench/moyodb-baseline.ts', globalThis.location.href).href),
                            import(new URL('/bench/indexeddb-baseline.ts', globalThis.location.href).href),
                            import(new URL('/bench/workloads.ts', globalThis.location.href).href)
                        ]);
                        for (const spec of scalingSpecs) {
                            if (WORKLOADS.some((workload) => workload.name === spec.name))
                                throw new Error(`Duplicate scaling workload ${spec.name}`);
                            const template = WORKLOADS.find((workload) => workload.name === 'large_value_64kb');
                            WORKLOADS.push({
                                ...template,
                                ...spec,
                                smoke: false,
                                tags: ['write', 'scaling']
                            });
                        }
                        for (const name of workloadNames) {
                            if (!WORKLOADS.some((workload) => workload.name === name))
                                throw new Error(`Unknown workload ${name}`);
                        }
                        globalThis.__abbaSampleOffset = 0;
                        for (const runner of [moyoDbBaseline, indexedDbBaseline]) {
                            const prepare = runner.prepare;
                            runner.prepare = (ctx) => {
                                ctx.sampleIndex += globalThis.__abbaSampleOffset;
                                return prepare(ctx);
                            };
                        }
                    },
                    { scalingSpecs, workloadNames }
                );
            }
            const cdp = await context.newCDPSession(pages.before);
            record.browserVersion = await cdp.send('Browser.getVersion');
            record.browserCommandLine = await cdp.send('Browser.getBrowserCommandLine');
            assert.ok(
                !record.browserCommandLine.arguments.includes('--incognito'),
                'Incognito is not a persistent comparison'
            );
            assert.ok(record.browserCommandLine.arguments.includes(`--user-data-dir=${profile}`));
            await cdp.detach();
            async function run(variant, warmup, offset, visit) {
                const report = await pages[variant].evaluate(
                    async (options) => {
                        globalThis.__abbaSampleOffset = options.offset;
                        return await globalThis.moyodbBench.runBenchmarkSuite(options.bench);
                    },
                    {
                        offset,
                        bench: {
                            profile: 'full',
                            engines: ['moyodb', 'indexeddb'],
                            workloadNames,
                            sampleCountOverride: warmup ? 1 : samples,
                            warmupCountOverride: warmup ? warmups - 1 : 0,
                            workloadTimeoutMs: timeoutMs,
                            persistentContext: true,
                            indexedDbDurability: 'strict',
                            gitSha: initial[variant].head + (initial[variant].trackedDiff ? '+local-changes' : ''),
                            dbNamePrefix: `abba-${Date.now()}-${launch}-${visit}-${variant}`
                        }
                    }
                );
                parity(report);
                const result = { variant, offset, report };
                (warmup ? record.warmups : record.visits).push(result);
                await save();
                console.log(
                    JSON.stringify({
                        launch,
                        visit,
                        variant,
                        warmup,
                        results: report.results.map((row) => ({
                            workload: row.workloadName,
                            engine: row.engine,
                            samples: row.rawSamples
                        }))
                    })
                );
            }
            if (warmups > 0) {
                for (const variant of [order[0], order[1]]) await run(variant, true, 0, 'warmup');
            }
            const nextOffset = {
                before: launch * samples * 2,
                after: launch * samples * 2
            };
            for (const [visit, variant] of order.entries()) {
                await run(variant, false, nextOffset[variant], visit);
                nextOffset[variant] += samples;
            }
            for (let visit = 0; visit < 2; visit += 1) {
                const before = record.visits.filter((item) => item.variant === 'before')[visit].report;
                const after = record.visits.filter((item) => item.variant === 'after')[visit].report;
                for (let row = 0; row < before.results.length; row += 1) {
                    assert.equal(before.results[row].workloadName, after.results[row].workloadName);
                    assert.equal(before.results[row].engine, after.results[row].engine);
                    for (const field of [
                        'recordCount',
                        'keySize',
                        'valueSize',
                        'batchSize',
                        'transactionBoundaries',
                        'sampleCount',
                        'warmupCount'
                    ]) {
                        assert.equal(
                            before.results[row][field],
                            after.results[row][field],
                            `cross-variant ${field} parity`
                        );
                    }
                    assert.deepEqual(
                        before.results[row].contentChecksums,
                        after.results[row].contentChecksums,
                        'cross-variant content parity'
                    );
                }
            }
            record.httpAssetsAfter = {};
            for (const variant of ['before', 'after'])
                record.httpAssetsAfter[variant] = await verifyAssets(context, urls[variant], initial[variant]);
            assert.deepEqual(record.browserErrors, []);
            record.hostAfter = await hostSnapshot();
            record.completed = true;
        } finally {
            await context?.close();
            for (const server of servers) await server.close();
            await rm(profile, { recursive: true, force: true });
            await rm(cacheRoot, { recursive: true, force: true });
            await save();
        }
    }
    for (const variant of ['before', 'after']) {
        const final = await fingerprint(roots[variant]);
        assert.deepEqual(final, initial[variant], `${variant} source or artifact changed during comparison`);
    }
    output.completed = true;
    await save();
    console.log(out);
} catch (error) {
    output.runnerError = error.stack ?? String(error);
    await save();
    throw error;
}
