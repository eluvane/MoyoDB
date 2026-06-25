#!/usr/bin/env node
import { access, mkdir, readdir, readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
    formatDeltaPercent,
    formatNumber,
    readCliOption,
    renderMarkdownTable,
    writeJsonFile,
    writeTextFile
} from './reporting.mjs';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const argv = process.argv.slice(2);
const previousPath = readCliOption(argv, '--previous')
    ? path.resolve(rootDir, readCliOption(argv, '--previous'))
    : undefined;
const currentPath = path.resolve(rootDir, readCliOption(argv, '--current') ?? '.benchmark/current/baseline.json');
const outDir = path.resolve(rootDir, readCliOption(argv, '--out-dir') ?? '.benchmark/comparison');
const failOnRegression = argv.includes('--fail-on-regression');
const benchmarkPolicy = await readBenchmarkPolicy();
const throughputRegressionPct =
    readNumericOption(argv, '--throughput-regression-pct') ?? benchmarkPolicy.thresholds?.throughputRegressionPct ?? 5;
const latencyRegressionPct =
    readNumericOption(argv, '--latency-regression-pct') ?? benchmarkPolicy.thresholds?.latencyRegressionPct ?? 5;
const tailLatencyRegressionPct =
    readNumericOption(argv, '--tail-latency-regression-pct') ??
    benchmarkPolicy.thresholds?.tailLatencyRegressionPct ??
    8;
await mkdir(outDir, { recursive: true });

const current = JSON.parse(await readFile(currentPath, 'utf8'));
const resolvedPreviousPath = await resolveDownloadedBaseline(previousPath);
const previousExists = await exists(resolvedPreviousPath);
if (!previousExists) {
    const summary = [
        '# MoyoDB benchmark comparison',
        '',
        'No previous benchmark baseline artifact was available for comparison.',
        'This is expected on the first run, after artifact expiry, or when the earlier run did not upload `benchmark-baseline`.',
        '',
        `Current baseline: ${path.relative(rootDir, currentPath)}`,
        ''
    ].join('\n');
    await writeTextFile(path.join(outDir, 'summary.md'), summary);
    await writeJsonFile(path.join(outDir, 'comparison.json'), {
        schemaVersion: 1,
        generatedAt: new Date().toISOString(),
        previousAvailable: false,
        thresholds: thresholdPolicy(),
        current,
        comparisons: []
    });
    process.exit(0);
}

const previous = JSON.parse(await readFile(resolvedPreviousPath, 'utf8'));
const currentCases = flattenCases(current);
const previousCases = flattenCases(previous);
const comparisons = [];
for (const [caseId, currentEntry] of currentCases) {
    const previousEntry = previousCases.get(caseId);
    if (!previousEntry) {
        continue;
    }
    const opsDeltaPct = deltaPercent(currentEntry.metrics.opsPerSec, previousEntry.metrics.opsPerSec);
    const avgLatencyDeltaPct = deltaPercent(currentEntry.metrics.avgLatencyMs, previousEntry.metrics.avgLatencyMs);
    const p95LatencyDeltaPct = deltaPercent(currentEntry.metrics.p95LatencyMs, previousEntry.metrics.p95LatencyMs);
    const p99LatencyDeltaPct = deltaPercent(currentEntry.metrics.p99LatencyMs, previousEntry.metrics.p99LatencyMs);
    const regressionReasons = [];
    if (Number.isFinite(opsDeltaPct) && opsDeltaPct <= -throughputRegressionPct) {
        regressionReasons.push(`throughput ${formatDeltaPercent(opsDeltaPct)}`);
    }
    if (Number.isFinite(avgLatencyDeltaPct) && avgLatencyDeltaPct >= latencyRegressionPct) {
        regressionReasons.push(`avg latency ${formatDeltaPercent(avgLatencyDeltaPct)}`);
    }
    if (Number.isFinite(p95LatencyDeltaPct) && p95LatencyDeltaPct >= latencyRegressionPct) {
        regressionReasons.push(`p95 latency ${formatDeltaPercent(p95LatencyDeltaPct)}`);
    }
    if (Number.isFinite(p99LatencyDeltaPct) && p99LatencyDeltaPct >= tailLatencyRegressionPct) {
        regressionReasons.push(`p99 latency ${formatDeltaPercent(p99LatencyDeltaPct)}`);
    }
    comparisons.push({
        id: caseId,
        suite: currentEntry.suite,
        label: currentEntry.label,
        previous: previousEntry.metrics,
        current: currentEntry.metrics,
        delta: {
            opsPerSecPct: opsDeltaPct,
            avgLatencyPct: avgLatencyDeltaPct,
            p95LatencyPct: p95LatencyDeltaPct,
            p99LatencyPct: p99LatencyDeltaPct
        },
        regression: regressionReasons.length > 0,
        regressionReasons
    });
}

const regressions = comparisons.filter((entry) => entry.regression);
const summaryLines = [
    '# MoyoDB benchmark comparison',
    '',
    `Current baseline: ${current.generatedAt}`,
    `Previous baseline: ${previous.generatedAt}`,
    `Thresholds: throughput -${throughputRegressionPct}%, avg/p95 +${latencyRegressionPct}%, p99 +${tailLatencyRegressionPct}%`,
    '',
    regressions.length === 0
        ? 'No regressions crossed the governance threshold.'
        : `Detected ${regressions.length} benchmark governance regression(s).`,
    ''
];
const suites = new Set(comparisons.map((entry) => entry.suite));
for (const suite of suites) {
    const rows = comparisons
        .filter((entry) => entry.suite === suite)
        .map((entry) => [
            entry.label,
            formatNumber(entry.previous.opsPerSec),
            formatNumber(entry.current.opsPerSec),
            formatDeltaPercent(entry.delta.opsPerSecPct),
            formatDeltaPercent(entry.delta.avgLatencyPct),
            formatDeltaPercent(entry.delta.p95LatencyPct),
            formatDeltaPercent(entry.delta.p99LatencyPct),
            entry.regression ? entry.regressionReasons.join('; ') : 'ok'
        ]);
    summaryLines.push(`## ${suite}`);
    summaryLines.push('');
    summaryLines.push(
        renderMarkdownTable(
            ['Case', 'prev ops/sec', 'curr ops/sec', 'Δ ops', 'Δ avg', 'Δ p95', 'Δ p99', 'status'],
            rows
        )
    );
    summaryLines.push('');
}
await writeTextFile(path.join(outDir, 'summary.md'), `${summaryLines.join('\n')}\n`);
await writeJsonFile(path.join(outDir, 'comparison.json'), {
    schemaVersion: 1,
    generatedAt: new Date().toISOString(),
    previousAvailable: true,
    previousGeneratedAt: previous.generatedAt,
    currentGeneratedAt: current.generatedAt,
    thresholds: thresholdPolicy(),
    comparisons
});
if (failOnRegression && regressions.length > 0) {
    console.error(`Benchmark regression gate failed with ${regressions.length} regression(s).`);
    process.exit(1);
}

async function exists(filePath) {
    if (!filePath) {
        return false;
    }
    try {
        await access(filePath);
        return true;
    } catch {
        return false;
    }
}

async function findFileRecursively(startDir, targetName) {
    try {
        const entries = await readdir(startDir, { withFileTypes: true });
        for (const entry of entries) {
            const candidate = path.join(startDir, entry.name);
            if (entry.isFile() && entry.name === targetName) {
                return candidate;
            }
            if (entry.isDirectory()) {
                const nested = await findFileRecursively(candidate, targetName);
                if (nested) {
                    return nested;
                }
            }
        }
    } catch {
        return undefined;
    }
    return undefined;
}

async function resolveDownloadedBaseline(filePath) {
    if (!filePath) {
        return undefined;
    }
    if (await exists(filePath)) {
        return filePath;
    }
    return findFileRecursively(path.dirname(filePath), path.basename(filePath));
}

function flattenCases(report) {
    return new Map(
        (report.suites ?? []).flatMap((suite) =>
            (suite.cases ?? [])
                .filter((entry) => entry?.metrics)
                .map((entry) => [
                    `${suite.suite}::${entry.id}`,
                    {
                        suite: suite.suite,
                        ...entry
                    }
                ])
        )
    );
}

function deltaPercent(currentValue, previousValue) {
    if (!Number.isFinite(currentValue) || !Number.isFinite(previousValue) || previousValue === 0) {
        return Number.NaN;
    }
    return ((currentValue - previousValue) / previousValue) * 100;
}

async function readBenchmarkPolicy() {
    try {
        return JSON.parse(await readFile(path.join(rootDir, '.benchmark', 'policy.json'), 'utf8'));
    } catch {
        return {};
    }
}

function readNumericOption(args, flag) {
    const raw = readCliOption(args, flag);
    if (raw === undefined) {
        return undefined;
    }
    const value = Number(raw);
    if (!Number.isFinite(value) || value < 0) {
        throw new Error(`${flag} must be a non-negative number`);
    }
    return value;
}

function thresholdPolicy() {
    return {
        throughputRegressionPct,
        latencyRegressionPct,
        tailLatencyRegressionPct,
        failOnRegression
    };
}
