import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { gunzipSync } from 'node:zlib';
import ts from 'typescript';
import { benchmarkCaseKey } from '../../../scripts/benchmark/reporting.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const temporaryRoot = resolve(here, '../../../.tmp');
await mkdir(temporaryRoot, { recursive: true });
const output = await mkdtemp(join(temporaryRoot, 'benchmark-datasets-'));
await mkdir(join(output, 'src'));
await mkdir(join(output, 'bench'));
await writeFile(
    join(output, 'src/index.mjs'),
    'export function openDB() { throw new Error("storage is not used by dataset tests"); }'
);
await writeFile(join(output, 'src/internal.mjs'), 'export function withTimeout(promise) { return promise; }');
try {
    for (const name of ['compression', 'snappy', 'codec']) {
        const source = await readFile(join(here, `../src/${name}.ts`), 'utf8');
        const result = ts.transpileModule(source, {
            fileName: `${name}.ts`,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
        });
        await writeFile(
            join(output, `src/${name}.mjs`),
            result.outputText.replace(/from '(\.[^']+)'/g, "from '$1.mjs'")
        );
    }
    for (const name of [
        'workloads',
        'report',
        'bench-runner',
        'moyodb-baseline',
        'indexeddb-baseline',
        'sync-access-handle'
    ]) {
        const source = await readFile(join(here, `../bench/${name}.ts`), 'utf8');
        const result = ts.transpileModule(source, {
            fileName: `${name}.ts`,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
        });
        const extra =
            name === 'moyodb-baseline'
                ? '\nexport { buildEntryBatches, measureCommit };\n'
                : name === 'indexeddb-baseline'
                  ? '\nexport { buildEntryBatches };\n'
                  : '';
        await writeFile(
            join(output, `bench/${name}.mjs`),
            result.outputText.replace(/from '(\.[^']+)'/g, "from '$1.mjs'") + extra
        );
    }
    const workload = await import(pathToFileURL(join(output, 'bench/workloads.mjs')).href);
    const moyo = await import(pathToFileURL(join(output, 'bench/moyodb-baseline.mjs')).href);
    const idb = await import(pathToFileURL(join(output, 'bench/indexeddb-baseline.mjs')).href);
    const runner = await import(pathToFileURL(join(output, 'bench/bench-runner.mjs')).href);
    const reports = await import(pathToFileURL(join(output, 'bench/report.mjs')).href);
    const snappy = await import(pathToFileURL(join(output, 'src/snappy.mjs')).href);
    const compression = await import(pathToFileURL(join(output, 'src/compression.mjs')).href);
    assert.deepEqual(workload.workloadPolicy(workload.WORKLOADS[0]).compressionTuning, {
        algorithm: 'snappy-raw-block',
        algorithmVersion: compression.STORE_VALUE_COMPRESSION_POLICY_VERSION,
        thresholdBytes: compression.STORE_VALUE_COMPRESSION_THRESHOLD,
        minimumSavingPercent: compression.STORE_VALUE_MIN_COMPRESSION_SAVING_PERCENT,
        envelopeBytes: compression.STORE_VALUE_COMPRESSION_ENVELOPE_BYTES,
        sampling: {
            minimumInputBytes: compression.STORE_VALUE_COMPRESSION_PREFLIGHT_MIN_BYTES,
            windowBytes: compression.STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_BYTES,
            windowCount: compression.STORE_VALUE_COMPRESSION_PREFLIGHT_WINDOW_COUNT,
            positioning: 'start-middle-end',
            profitability: 'aggregate-size-plus-one-envelope'
        }
    });
    assert.deepEqual(await moyo.moyoDbBaseline.metrics({ dbName: 'diagnostic-without-prepared-db' }), {});
    const legacy = workload.valueBytes(19, 1024);
    assert.deepEqual(legacy.subarray(0, 256), legacy.subarray(256, 512));
    assert.deepEqual([...workload.valueBytes(0, 8)], [108, 219, 126, 197, 96, 63, 146, 201]);
    const entropy = workload.valueBytes(19, 65_536, 'high-entropy-binary');
    assert.notDeepEqual(entropy.subarray(0, 256), entropy.subarray(256, 512));
    assert.deepEqual(entropy, workload.valueBytes(19, 65_536, 'high-entropy-binary'));
    assert.notDeepEqual(entropy, workload.valueBytes(20, 65_536, 'high-entropy-binary'));
    const counts = new Uint32Array(256);
    for (const byte of entropy) counts[byte] += 1;
    const bits = counts.reduce(
        (sum, count) => (count === 0 ? sum : sum - (count / entropy.length) * Math.log2(count / entropy.length)),
        0
    );
    assert.ok(bits > 7.9, `byte entropy is ${bits}`);
    assert.ok(snappy.encodeSnappy(entropy).length > entropy.length * 0.95);
    const compressible = workload.valueBytes(19, 65_536);
    assert.ok(snappy.encodeSnappy(compressible).length < compressible.length * 0.1);
    const precompressed = workload.valueBytes(19, 65_536, 'precompressed');
    assert.ok(snappy.encodeSnappy(precompressed).length > precompressed.length * 0.95);

    for (const profile of workload.DATASET_PROFILES) {
        for (const size of [1023, 1024, 1025, 4066, 65_536]) {
            const spec = workload.datasetWorkload(profile, size, 'snappy');
            const expected = workload.valueBytes(7, size, profile);
            assert.equal(expected.length, size);
            const a = moyo.buildEntryBatches(spec, 8, 4);
            const b = idb.buildEntryBatches(spec, 8, 4);
            assert.deepEqual(a, b);
            assert.deepEqual(a[1][3][1], expected);
            const checksum = new workload.ContentChecksum();
            checksum.add(expected);
            assert.equal(checksum.digest(), workload.expectedValuesChecksum(spec, [7]));
            if (profile === 'realistic-json') assert.equal(JSON.parse(new TextDecoder().decode(expected)).id, 7);
            if (profile === 'realistic-text') assert.match(new TextDecoder().decode(expected), /Order 7:/);
            if (profile === 'precompressed') assert.ok(gunzipSync(expected).length > 0);
        }
    }
    for (const size of [23, 65_558, 65_559, 65_560, 65_563, 65_564, 131_098, 1_048_576]) {
        const encoded = workload.valueBytes(17, size, 'precompressed');
        assert.equal(encoded.length, size);
        assert.deepEqual(
            gunzipSync(encoded),
            Buffer.from(workload.valueBytes(17, gunzipSync(encoded).length, 'high-entropy-binary'))
        );
    }
    assert.throws(() => workload.valueBytes(1, 256, 'unknown'), TypeError);
    assert.equal(
        workload.selectWorkloads('full').some((spec) => spec.tags?.includes('dataset')),
        false
    );

    const context = Object.fromEntries(
        [
            'profile',
            'browserName',
            'browserVersion',
            'persistentContext',
            'indexedDbDurability',
            'moyoDbDurability',
            'sdkBuildMode',
            'wasmBuildMode',
            'backendPath',
            'headless',
            'recordCount',
            'keySize',
            'valueSize',
            'batchSize',
            'transactionBoundaries',
            'warmupCount',
            'sampleCount'
        ].map((field) => [field, 1])
    );
    context.benchmarkPolicy = workload.workloadPolicy(workload.WORKLOADS[0]);
    const baseKey = benchmarkCaseKey('browser', { id: 'case', comparisonContext: context });
    assert.equal(typeof baseKey, 'string');
    for (const change of [
        (policy) => {
            policy.dataset.profile = 'high-entropy-binary';
        },
        (policy) => {
            policy.dataset.seed += 1;
        },
        (policy) => {
            policy.dataset.version = 2;
        },
        (policy) => {
            policy.compression = 'snappy';
        },
        (policy) => {
            policy.compressionTuning.thresholdBytes = 2048;
        },
        (policy) => {
            policy.compressionTuning.minimumSavingPercent = 20;
        },
        (policy) => {
            policy.compressionTuning.algorithm = 'unknown';
        },
        (policy) => {
            policy.compressionTuning.algorithmVersion = 1;
        },
        (policy) => {
            policy.compressionTuning.envelopeBytes = 19;
        },
        (policy) => {
            policy.compressionTuning.sampling.minimumInputBytes = 131_072;
        },
        (policy) => {
            policy.compressionTuning.sampling.windowBytes = 2048;
        },
        (policy) => {
            policy.compressionTuning.sampling.windowCount = 4;
        },
        (policy) => {
            policy.compressionTuning.sampling.positioning = 'prefix-only';
        },
        (policy) => {
            policy.compressionTuning.sampling.profitability = 'each-window';
        },
        (policy) => {
            policy.changeFeed.enabled = false;
        },
        (policy) => {
            policy.changeFeed.retainTxids = 1;
        },
        (policy) => {
            policy.timing = 'unknown';
        }
    ]) {
        const modified = structuredClone(context);
        change(modified.benchmarkPolicy);
        assert.notEqual(benchmarkCaseKey('browser', { id: 'case', comparisonContext: modified }), baseKey);
    }
    for (const policy of [
        undefined,
        {},
        { dataset: {} },
        { ...context.benchmarkPolicy, compression: 'unknown' },
        { ...context.benchmarkPolicy, compressionTuning: undefined },
        {
            ...context.benchmarkPolicy,
            compressionTuning: { thresholdBytes: 1024, minimumSavingPercent: 10 }
        },
        {
            ...context.benchmarkPolicy,
            compressionTuning: { ...context.benchmarkPolicy.compressionTuning, sampling: undefined }
        },
        { ...context.benchmarkPolicy, changeFeed: { enabled: true, retainTxids: 0 } }
    ]) {
        assert.equal(
            benchmarkCaseKey('browser', { id: 'case', comparisonContext: { ...context, benchmarkPolicy: policy } }),
            undefined
        );
    }

    const originalPerformance = Object.getOwnPropertyDescriptor(globalThis, 'performance');
    let clock = 0;
    let measuredSample;
    Object.defineProperty(globalThis, 'performance', { configurable: true, value: { now: () => clock } });
    try {
        const collected = { explicitCommitMs: null, explicitCommitCount: 0, closeMs: null, mainBytes: 4096 };
        const phases = [];
        const fakeRunner = {
            engine: 'moyodb',
            async prepare() {
                clock += 100;
                return async () => {
                    phases.push('close');
                    clock += 11;
                    collected.closeMs = 11;
                };
            },
            async run() {
                phases.push('run');
                clock += 4;
                await moyo.measureCommit(
                    {
                        async commit() {
                            clock += 3;
                        }
                    },
                    collected
                );
            },
            async metrics() {
                phases.push('metrics');
                clock += 13;
                return collected;
            },
            async verify() {
                phases.push('verify');
                clock += 17;
                return 'verified';
            },
            async cleanup() {
                phases.push('cleanup');
                clock += 19;
            }
        };
        const result = await runner.runPreparedSample(fakeRunner, { workload: workload.WORKLOADS[0] }, undefined);
        measuredSample = result;
        assert.equal(result.elapsed, 7);
        assert.equal(result.metrics.explicitCommitMs, 3);
        assert.equal(result.metrics.verificationMs, 17);
        assert.equal(result.metrics.closeMs, 11);
        assert.equal(result.metrics.cleanupMs, 30);
        assert.equal(result.metrics.backendReads, null);
        assert.deepEqual(phases, ['run', 'metrics', 'verify', 'close', 'cleanup']);
    } finally {
        Object.defineProperty(globalThis, 'performance', originalPerformance);
    }
    const report = {
        schemaVersion: 1,
        generatedAt: 'fixture',
        profile: 'smoke',
        browser: { name: 'fixture', version: '1', userAgent: 'fixture' },
        environment: {},
        notes: [],
        results: [
            {
                engine: 'moyodb',
                workloadName: 'phase-fixture',
                policy: context.benchmarkPolicy,
                status: 'ok',
                rawSamples: [measuredSample.elapsed],
                sampleMetrics: [measuredSample.metrics],
                warmupSamples: [],
                contentChecksums: [],
                stats: { p50: 7 },
                notes: ''
            }
        ]
    };
    await mkdir(join(output, 'results'));
    await writeFile(join(output, 'results/fixture.json'), JSON.stringify(report));
    await writeFile(
        join(output, 'generate-report.mjs'),
        await readFile(join(here, '../bench/generate-report.mjs'), 'utf8')
    );
    await import(pathToFileURL(join(output, 'generate-report.mjs')).href);
    for (const markdown of [
        await readFile(join(output, 'results/report.md'), 'utf8'),
        reports.renderMarkdownReport(report)
    ]) {
        assert.match(
            markdown,
            /lcg-repeat-256 \| false \| true \| 3\.00 \| 11\.00 \| 30\.00 \| n\/a \| 4096\.00 \| n\/a/
        );
        assert.match(markdown, /autocommit internals and backend I\/O counts are unavailable/);
    }
    console.info('Benchmark dataset, byte parity, policy governance and phase timing checks passed.');
} finally {
    await rm(output, { recursive: true, force: true });
}
