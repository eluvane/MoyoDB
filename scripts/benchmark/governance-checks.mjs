import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const scripts = ['collect.mjs', 'compare.mjs', 'reporting.mjs'];

function fixture(t) {
    const directory = mkdtempSync(join(tmpdir(), 'moyo-bench-governance-'));
    t.after(() => rmSync(directory, { recursive: true, force: true }));
    const destination = join(directory, 'scripts', 'benchmark');
    mkdirSync(destination, { recursive: true });
    for (const name of scripts) {
        copyFileSync(join(here, name), join(destination, name));
    }
    mkdirSync(join(directory, 'packages', 'sdk', 'bench', 'results'), { recursive: true });
    return directory;
}

function write(directory, file, value) {
    const destination = join(directory, file);
    mkdirSync(dirname(destination), { recursive: true });
    writeFileSync(destination, typeof value === 'string' ? value : `${JSON.stringify(value, null, 2)}\n`);
}

function run(directory, script, args = []) {
    const result = spawnSync(process.execPath, [join(directory, script), ...args], {
        cwd: directory,
        encoding: 'utf8'
    });
    assert.ifError(result.error);
    return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

function report(rows) {
    return {
        schemaVersion: 1,
        profile: 'full',
        browser: { name: 'chromium', version: '123' },
        environment: {
            persistentContext: false,
            indexedDbDurability: 'strict',
            moyoDbDurability: 'strict',
            sdkBuildMode: 'production',
            wasmBuildMode: 'release',
            backendPath: 'opfs',
            headless: true
        },
        results: rows.map((row) => ({
            status: row.status ?? 'ok',
            engine: 'moyodb',
            workloadName: row.name,
            recordCount: 1,
            keySize: 16,
            valueSize: 8,
            batchSize: 1,
            transactionBoundaries: 'one',
            policy: {
                dataset: { profile: 'lcg-repeat-256', version: 1, seed: 0 },
                compression: false,
                compressionTuning: {
                    algorithm: 'snappy-raw-block',
                    algorithmVersion: 2,
                    thresholdBytes: 1024,
                    minimumSavingPercent: 10,
                    envelopeBytes: 18,
                    sampling: {
                        minimumInputBytes: 65_536,
                        windowBytes: 1024,
                        windowCount: 3,
                        positioning: 'start-middle-end',
                        profitability: 'aggregate-size-plus-one-envelope'
                    }
                },
                changeFeed: { enabled: true, retainTxids: 100_000 },
                timing: 'run-callback-only-v1'
            },
            warmupCount: 0,
            sampleCount: 1,
            stats:
                (row.status ?? 'ok') === 'ok'
                    ? { mean: row.mean, p50: row.mean, p95: row.mean, p99: row.mean }
                    : undefined,
            error: row.error
        }))
    };
}

function collect(directory, rows, outDir) {
    write(directory, 'packages/sdk/bench/results/browser.json', report(rows));
    return run(directory, 'scripts/benchmark/collect.mjs', ['--from-results', '--out-dir', outDir]);
}

test('a zero-latency baseline fails closed when latency becomes measurable', (t) => {
    const directory = fixture(t);
    assert.equal(collect(directory, [{ name: 'put', mean: 0 }], 'previous').status, 0);
    const previous = JSON.parse(readFileSync(join(directory, 'previous/baseline.json'), 'utf8'));
    assert.equal(previous.suites[0].cases[0].metrics.opsPerSec, null);
    assert.equal(collect(directory, [{ name: 'put', mean: 50 }], 'current').status, 0);
    const gate = run(directory, 'scripts/benchmark/compare.mjs', [
        '--previous',
        'previous/baseline.json',
        '--current',
        'current/baseline.json',
        '--out-dir',
        'comparison',
        '--fail-on-regression'
    ]);
    assert.equal(gate.status, 1, gate.output);
    assert.match(gate.output, /failed with 1 regression/u);
    const comparison = JSON.parse(readFileSync(join(directory, 'comparison/comparison.json'), 'utf8'));
    assert.equal(comparison.comparisons[0].regression, true);
    assert.match(comparison.comparisons[0].regressionReasons.join(' '), /avg latency \+Infinity%/u);
});

test('timer-resolution samples are not reported as a throughput collapse', (t) => {
    const directory = fixture(t);
    assert.equal(collect(directory, [{ name: 'put', mean: 10 }], 'previous').status, 0);
    assert.equal(collect(directory, [{ name: 'put', mean: 0 }], 'current').status, 0);
    const gate = run(directory, 'scripts/benchmark/compare.mjs', [
        '--previous',
        'previous/baseline.json',
        '--current',
        'current/baseline.json',
        '--out-dir',
        'comparison',
        '--fail-on-regression'
    ]);
    assert.equal(gate.status, 0, gate.output);
    const comparison = JSON.parse(readFileSync(join(directory, 'comparison/comparison.json'), 'utf8'));
    assert.equal(comparison.comparisons[0].regression, false);
});

test('failed browser rows and invalid policy fail the benchmark gate', (t) => {
    const directory = fixture(t);
    const failed = collect(directory, [{ name: 'put', mean: 10, status: 'error', error: 'boom' }], 'broken');
    assert.equal(failed.status, 1, failed.output);
    assert.match(failed.output, /browser benchmark row failed: moyodb\/put: boom/u);
    assert.equal(collect(directory, [{ name: 'put', mean: 10 }], 'current').status, 0);
    write(directory, '.benchmark/policy.json', '{');
    const gate = run(directory, 'scripts/benchmark/compare.mjs', [
        '--current',
        'current/baseline.json',
        '--out-dir',
        'comparison',
        '--fail-on-regression'
    ]);
    assert.equal(gate.status, 1, gate.output);
    assert.match(gate.output, /JSON|policy\.json/u);
});

test('a disappeared baseline case fails the regression gate', (t) => {
    const directory = fixture(t);
    assert.equal(
        collect(
            directory,
            [
                { name: 'put', mean: 10 },
                { name: 'get', mean: 10 }
            ],
            'previous'
        ).status,
        0
    );
    assert.equal(collect(directory, [{ name: 'put', mean: 10 }], 'current').status, 0);
    const gate = run(directory, 'scripts/benchmark/compare.mjs', [
        '--previous',
        'previous/baseline.json',
        '--current',
        'current/baseline.json',
        '--out-dir',
        'comparison',
        '--fail-on-regression'
    ]);
    assert.equal(gate.status, 1, gate.output);
    assert.match(gate.output, /1 baseline case\(s\) were absent/u);
    const comparison = JSON.parse(readFileSync(join(directory, 'comparison/comparison.json'), 'utf8'));
    assert.equal(comparison.comparisons.length, 1);
    assert.equal(comparison.missingCases.length, 1);
    assert.equal(comparison.missingCases[0].id, 'moyodb:get');
});

test('invalid suites.json fails collection instead of publishing an empty suite list', (t) => {
    const directory = fixture(t);
    write(directory, '.benchmark/suites.json', '{');
    const result = collect(directory, [{ name: 'put', mean: 10 }], 'current');
    assert.equal(result.status, 1, result.output);
    assert.match(result.output, /JSON/u);
});

test('a value flag without a value fails instead of consuming the next flag', (t) => {
    const directory = fixture(t);
    const result = run(directory, 'scripts/benchmark/compare.mjs', ['--out-dir', '--fail-on-regression']);
    assert.equal(result.status, 1, result.output);
    assert.match(result.output, /--out-dir requires a value/u);
});

test('unknown workload names are rejected and known manual names can be selected', async () => {
    const { selectWorkloads } = await import(
        pathToFileURL(join(here, '..', '..', 'packages', 'sdk', 'bench', 'workloads.ts')).href
    );
    assert.throws(
        () => selectWorkloads('smoke', ['not_a_workload']),
        /Unknown benchmark workload\(s\): not_a_workload/u
    );
    const selected = selectWorkloads('smoke', ['bulk_insert_1m', 'open_empty_db']);
    assert.deepEqual(
        selected.map((workload) => workload.name),
        ['open_empty_db', 'bulk_insert_1m']
    );
});

test('report generation explains a missing results directory', (t) => {
    const directory = fixture(t);
    copyFileSync(
        join(here, '..', '..', 'packages', 'sdk', 'bench', 'generate-report.mjs'),
        join(directory, 'generate-report.mjs')
    );
    const result = run(directory, 'generate-report.mjs');
    assert.equal(result.status, 0, result.output);
    assert.match(readFileSync(join(directory, 'results', 'report.md'), 'utf8'), /No raw browser benchmark JSON/u);
});
