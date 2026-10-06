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
    'warmupCount',
    'sampleCount'
];

export function benchmarkCaseKey(suite, entry) {
    const context = entry.comparisonContext;
    if (!context || !comparisonFields.every((field) => Object.hasOwn(context, field))) {
        return undefined;
    }
    return JSON.stringify([suite, entry.id, ...comparisonFields.map((field) => context[field])]);
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
