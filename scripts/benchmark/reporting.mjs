import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';

export function readCliOption(argv, flag) {
    const index = argv.indexOf(flag);
    if (index < 0) {
        return undefined;
    }
    const value = argv[index + 1];
    if (value === undefined || value.startsWith('--')) {
        throw new Error(`${flag} requires a value`);
    }
    return value;
}

const comparisonFields = [
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
    'benchmarkPolicy',
    'warmupCount',
    'sampleCount'
];

export function benchmarkCaseKey(suite, entry) {
    const context = entry.comparisonContext;
    if (!context || !comparisonFields.every((field) => Object.hasOwn(context, field))) {
        return undefined;
    }
    if (!validBenchmarkPolicy(context.benchmarkPolicy)) return undefined;
    const policy = context.benchmarkPolicy;
    const canonicalPolicy = [
        policy.dataset.profile,
        policy.dataset.version,
        policy.dataset.seed,
        policy.compression,
        policy.compressionTuning.algorithm,
        policy.compressionTuning.algorithmVersion,
        policy.compressionTuning.thresholdBytes,
        policy.compressionTuning.minimumSavingPercent,
        policy.compressionTuning.envelopeBytes,
        policy.compressionTuning.sampling.minimumInputBytes,
        policy.compressionTuning.sampling.windowBytes,
        policy.compressionTuning.sampling.windowCount,
        policy.compressionTuning.sampling.positioning,
        policy.compressionTuning.sampling.profitability,
        policy.changeFeed.enabled,
        policy.changeFeed.retainTxids,
        policy.timing
    ];
    return JSON.stringify([
        suite,
        entry.id,
        ...comparisonFields.map((field) => (field === 'benchmarkPolicy' ? canonicalPolicy : context[field]))
    ]);
}

function validBenchmarkPolicy(policy) {
    return (
        policy &&
        ['lcg-repeat-256', 'high-entropy-binary', 'realistic-json', 'realistic-text', 'precompressed'].includes(
            policy.dataset?.profile
        ) &&
        policy.dataset.version === 1 &&
        Number.isSafeInteger(policy.dataset.seed) &&
        [false, 'snappy', 'gzip', 'deflate'].includes(policy.compression) &&
        policy.compressionTuning?.algorithm === 'snappy-raw-block' &&
        Number.isSafeInteger(policy.compressionTuning.algorithmVersion) &&
        policy.compressionTuning.algorithmVersion > 0 &&
        Number.isSafeInteger(policy.compressionTuning?.thresholdBytes) &&
        policy.compressionTuning.thresholdBytes >= 0 &&
        Number.isFinite(policy.compressionTuning.minimumSavingPercent) &&
        policy.compressionTuning.minimumSavingPercent >= 0 &&
        policy.compressionTuning.minimumSavingPercent <= 100 &&
        Number.isSafeInteger(policy.compressionTuning.envelopeBytes) &&
        policy.compressionTuning.envelopeBytes > 0 &&
        Number.isSafeInteger(policy.compressionTuning.sampling?.minimumInputBytes) &&
        policy.compressionTuning.sampling.minimumInputBytes > 0 &&
        Number.isSafeInteger(policy.compressionTuning.sampling.windowBytes) &&
        policy.compressionTuning.sampling.windowBytes > 0 &&
        policy.compressionTuning.sampling.windowBytes <= policy.compressionTuning.sampling.minimumInputBytes &&
        Number.isSafeInteger(policy.compressionTuning.sampling.windowCount) &&
        policy.compressionTuning.sampling.windowCount > 0 &&
        policy.compressionTuning.sampling.positioning === 'start-middle-end' &&
        policy.compressionTuning.sampling.profitability === 'aggregate-size-plus-one-envelope' &&
        typeof policy.changeFeed?.enabled === 'boolean' &&
        (policy.changeFeed.retainTxids === null ||
            (Number.isSafeInteger(policy.changeFeed.retainTxids) && policy.changeFeed.retainTxids > 0)) &&
        policy.timing === 'run-callback-only-v1'
    );
}

export async function writeJsonFile(filePath, value) {
    await writeTextFile(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

export async function writeTextFile(filePath, value) {
    await mkdir(path.dirname(filePath), { recursive: true });
    await writeFile(filePath, value, 'utf8');
}

export function formatNumber(value, fractionDigits = 2) {
    return Number.isFinite(value) ? value.toFixed(fractionDigits) : 'n/a';
}

export function formatDeltaPercent(value) {
    if (value === Number.POSITIVE_INFINITY) {
        return '+Infinity%';
    }
    if (value === Number.NEGATIVE_INFINITY) {
        return '-Infinity%';
    }
    if (!Number.isFinite(value)) {
        return 'n/a';
    }
    const prefix = value > 0 ? '+' : '';
    return `${prefix}${value.toFixed(2)}%`;
}

export function renderMarkdownTable(headers, rows) {
    const header = `| ${headers.join(' | ')} |`;
    const separator = `| ${headers.map(() => '---').join(' | ')} |`;
    const body = rows.map((row) => `| ${row.map(escapeCell).join(' | ')} |`);
    return [header, separator, ...body].join('\n');
}

function escapeCell(value) {
    return String(value).replaceAll('|', '/').replaceAll(/\s+/g, ' ').trim();
}
