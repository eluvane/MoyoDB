import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';

export function readCliOption(argv, flag) {
    const index = argv.indexOf(flag);
    return index < 0 ? undefined : argv[index + 1];
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
