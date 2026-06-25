#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { copyFile, mkdir, readdir, readFile, rm } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { formatNumber, readCliOption, renderMarkdownTable, writeJsonFile, writeTextFile } from './reporting.mjs';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const argv = process.argv.slice(2);
const smoke = argv.includes('--smoke');
const fromResults = argv.includes('--from-results');
const skipBrowser = argv.includes('--skip-browser');
const skipRust = argv.includes('--skip-rust');
const runRust = argv.includes('--run-rust');
const outDir = path.resolve(
    rootDir,
    readCliOption(argv, '--out-dir') ?? (smoke ? '.benchmark/smoke' : '.benchmark/current')
);
const resultsDir = path.join(rootDir, 'packages', 'sdk', 'bench', 'results');
await mkdir(outDir, { recursive: true });

const commands = [];
if (!fromResults) {
    if (!skipRust) {
        commands.push(runRust ? 'cargo bench --workspace' : 'cargo bench --workspace --no-run');
        await run('cargo', ['bench', '--workspace', ...(runRust ? [] : ['--no-run'])]);
    }
    if (!skipBrowser) {
        await emptyDirectory(resultsDir);
        commands.push('npm run bench:browser --workspace @moyodb/sdk');
        await run('npm', ['run', 'bench:browser', '--workspace', '@moyodb/sdk'], {
            MOYODB_BENCH_PROFILE: smoke ? 'smoke' : (process.env.MOYODB_BENCH_PROFILE ?? 'standard'),
            MOYODB_BENCH_SAMPLE_COUNT: smoke
                ? (process.env.MOYODB_BENCH_SAMPLE_COUNT ?? '3')
                : (process.env.MOYODB_BENCH_SAMPLE_COUNT ?? ''),
            MOYODB_BENCH_WARMUP_COUNT: smoke
                ? (process.env.MOYODB_BENCH_WARMUP_COUNT ?? '1')
                : (process.env.MOYODB_BENCH_WARMUP_COUNT ?? '')
        });
        commands.push('npm run bench:report --workspace @moyodb/sdk');
        await run('npm', ['run', 'bench:report', '--workspace', '@moyodb/sdk']);
    }
}

const suiteMetadata = await readSuiteMetadata();
const reports = await readBrowserReports();
if (!fromResults && !skipBrowser && reports.length === 0) {
    throw new Error('browser benchmark command completed but did not produce packages/sdk/bench/results/*.json');
}

await copyRawReports(reports);
const suites = buildSuites(reports);
const baseline = {
    schemaVersion: 1,
    project: 'MoyoDB',
    generatedAt: new Date().toISOString(),
    mode: smoke ? 'smoke' : fromResults ? 'from-results' : 'full',
    runtime: {
        node: process.version,
        platform: process.platform,
        arch: process.arch
    },
    benchmarkSuites: suiteMetadata.suites ?? [],
    commands,
    notes: [
        'Native Rust Criterion timings and browser SDK/OPFS timings measure different layers and must not be compared as equivalent paths.',
        'Browser rows are normalized from packages/sdk/bench/results raw JSON; skipped rows are omitted from regression comparison metrics.',
        'The Rust step is a benchmark compile gate by default; pass --run-rust when a local machine is intended to execute Criterion timings.'
    ],
    suites
};

await writeJsonFile(path.join(outDir, 'baseline.json'), baseline);
await writeTextFile(path.join(outDir, 'summary.md'), renderSummary(baseline));

function buildSuites(browserReports) {
    const cases = [];
    for (const { file, report } of browserReports) {
        for (const result of report.results ?? []) {
            if (result.status !== 'ok' || !result.stats) {
                continue;
            }
            const mean = Number(result.stats.mean);
            cases.push({
                id: `${result.engine}:${result.workloadName}`,
                label: `${result.engine} / ${result.workloadName}`,
                source: file,
                metrics: {
                    opsPerSec: mean > 0 ? 1000 / mean : 0,
                    avgLatencyMs: mean,
                    p50LatencyMs: Number(result.stats.p50),
                    p95LatencyMs: Number(result.stats.p95),
                    p99LatencyMs: Number(result.stats.p99)
                }
            });
        }
    }
    return [
        {
            suite: 'browser-sdk-wasm-opfs',
            description: 'Browser SDK, Worker, WASM, OPFS and IndexedDB comparison rows from packages/sdk/bench.',
            cases
        }
    ];
}

function renderSummary(baseline) {
    const lines = [
        '# MoyoDB benchmark baseline',
        '',
        `Generated at: ${baseline.generatedAt}`,
        `Mode: ${baseline.mode}`,
        `Node: ${baseline.runtime.node}`,
        `Platform: ${baseline.runtime.platform}/${baseline.runtime.arch}`,
        '',
        '## Policy',
        '',
        '- Native Rust Criterion timings are engine-core microbenchmarks.',
        '- Browser timings include SDK, Worker, WASM and storage-backend overhead.',
        '- Compare only matching workloads, browser profiles, sample counts and persistence modes.',
        ''
    ];
    for (const suite of baseline.suites) {
        lines.push(`## ${suite.suite}`);
        lines.push('');
        if (suite.cases.length === 0) {
            lines.push('No comparable browser benchmark rows were collected.');
            lines.push('');
            continue;
        }
        lines.push(
            renderMarkdownTable(
                ['Case', 'ops/sec', 'avg ms', 'p50 ms', 'p95 ms', 'p99 ms', 'source'],
                suite.cases.map((entry) => [
                    entry.label,
                    formatNumber(entry.metrics.opsPerSec),
                    formatNumber(entry.metrics.avgLatencyMs),
                    formatNumber(entry.metrics.p50LatencyMs),
                    formatNumber(entry.metrics.p95LatencyMs),
                    formatNumber(entry.metrics.p99LatencyMs),
                    entry.source
                ])
            )
        );
        lines.push('');
    }
    return `${lines.join('\n')}\n`;
}

async function readSuiteMetadata() {
    try {
        return JSON.parse(await readFile(path.join(rootDir, '.benchmark', 'suites.json'), 'utf8'));
    } catch {
        return {};
    }
}

async function readBrowserReports() {
    let files = [];
    try {
        files = (await readdir(resultsDir)).filter((name) => name.endsWith('.json')).sort();
    } catch {
        return [];
    }
    const reports = [];
    for (const file of files) {
        const absolute = path.join(resultsDir, file);
        try {
            const report = JSON.parse(await readFile(absolute, 'utf8'));
            if (report?.schemaVersion === 1 && Array.isArray(report.results)) {
                reports.push({ file, absolute, report });
            }
        } catch (error) {
            console.warn(`skipping invalid benchmark JSON ${file}: ${error.message}`);
        }
    }
    return reports;
}

async function copyRawReports(reports) {
    const rawDir = path.join(outDir, 'raw', 'browser');
    await mkdir(rawDir, { recursive: true });
    for (const report of reports) {
        await copyFile(report.absolute, path.join(rawDir, report.file));
    }
    const reportPath = path.join(resultsDir, 'report.md');
    try {
        await copyFile(reportPath, path.join(outDir, 'browser-report.md'));
    } catch {
        // The report is optional when collecting from pre-existing raw JSON.
    }
}

async function emptyDirectory(directory) {
    await mkdir(directory, { recursive: true });
    for (const entry of await readdir(directory)) {
        await rm(path.join(directory, entry), { recursive: true, force: true });
    }
}

async function run(command, args, env = {}) {
    await new Promise((resolve, reject) => {
        const isWindowsNpm = process.platform === 'win32' && command === 'npm';
        const executable = isWindowsNpm ? 'cmd.exe' : command;
        const spawnArgs = isWindowsNpm ? ['/d', '/s', '/c', 'npm.cmd', ...args] : args;
        const child = spawn(executable, spawnArgs, {
            cwd: rootDir,
            stdio: 'inherit',
            env: Object.fromEntries(Object.entries({ ...process.env, ...env }).filter(([, value]) => value !== ''))
        });
        child.once('error', reject);
        child.once('exit', (code) => {
            if (code === 0) {
                resolve();
                return;
            }
            reject(new Error(`${command} ${args.join(' ')} failed with exit code ${code ?? 'unknown'}`));
        });
    });
}
