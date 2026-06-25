import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const resultsDir = new URL('./results/', import.meta.url);
const outPath = new URL('./results/report.md', import.meta.url);
const files = (await readdir(resultsDir)).filter((name) => name.endsWith('.json')).sort();

if (files.length === 0) {
    const empty = `# Browser benchmark report\n\nNo raw browser benchmark JSON files were found in \`packages/sdk/bench/results\`.\n\nRun one of:\n\n\`\`\`bash\ncd packages/sdk\nnpm run bench:browser\nnpm run bench:indexeddb\nnpm run bench:opfs\n\`\`\`\n`;
    await writeFile(outPath, empty);
    console.log(`wrote ${join('bench', 'results', 'report.md')} (no results found)`);
    process.exit(0);
}

const reports = [];
for (const file of files) {
    const raw = await readFile(new URL(file, resultsDir), 'utf8');
    try {
        const parsed = JSON.parse(raw);
        if (parsed?.schemaVersion === 1 && Array.isArray(parsed.results)) {
            reports.push({ file, report: parsed });
        }
    } catch (error) {
        console.warn(`skipping invalid JSON result ${file}: ${error.message}`);
    }
}

const lines = [];
lines.push('# Browser benchmark report');
lines.push('');
lines.push(`Generated from ${reports.length} raw result file(s).`);
lines.push('');
lines.push('Native Rust engine microbench: not included here; run `cargo bench`.');
lines.push('WASM/Worker diagnostic benches: included for selected diagnostic workloads.');
lines.push(
    'Browser SDK bench / OPFS persistence bench / IndexedDB comparison bench: included below when raw browser results exist.'
);
lines.push(
    'Worker transport overhead bench: included for `worker_roundtrip_overhead` or `noop_worker_roundtrip_10k` when present.'
);
lines.push('');
lines.push('## Environments');
lines.push('');
lines.push('| File | Browser | SDK mode | WASM mode | Backend | OPFS | SyncAccessHandle | Persistent context |');
lines.push('| ---- | ------- | -------- | --------- | ------- | ---- | ---------------- | ------------------ |');

for (const { file, report } of reports) {
    const env = report.environment ?? {};
    const browser = env.browser ?? report.browser ?? {};
    lines.push(
        [
            file,
            `${browser.name ?? 'unknown'} ${browser.version ?? ''}`.trim(),
            tableText(env.sdkBuildMode ?? 'unknown'),
            tableText(env.wasmBuildMode ?? 'unknown'),
            tableText(env.backendPath ?? 'unknown'),
            String(env.opfsSupported ?? 'unknown'),
            String(env.syncAccessHandleSupported ?? 'unknown'),
            String(env.persistentContext ?? 'unknown')
        ]
            .join(' | ')
            .replace(/^/, '| ')
            .replace(/$/, ' |')
    );
}

lines.push('');
lines.push('## Results');
lines.push('');
lines.push(
    '| File | Generated | Browser | Profile | Engine | Workload | Status | warmups | n | p50 ms | p95 ms | p99 ms | mean ms | Notes |'
);
lines.push(
    '| ---- | --------- | ------- | ------- | ------ | -------- | ------ | ------- | - | ------ | ------ | ------ | ------- | ----- |'
);

for (const { file, report } of reports) {
    for (const result of report.results) {
        const stats = result.stats ?? {};
        lines.push(
            [
                file,
                report.generatedAt ?? result.timestamp ?? 'unknown',
                `${result.browser?.name ?? report.browser?.name ?? 'unknown'} ${result.browser?.version ?? report.browser?.version ?? ''}`.trim(),
                report.profile ?? 'unknown',
                result.engine,
                result.workloadName,
                result.status,
                String(result.warmupSamples?.length ?? 0),
                String(result.rawSamples?.length ?? 0),
                metric(stats.p50),
                metric(stats.p95),
                metric(stats.p99),
                metric(stats.mean),
                tableText(result.error ? `${result.notes}; ${result.error}` : result.notes)
            ]
                .join(' | ')
                .replace(/^/, '| ')
                .replace(/$/, ' |')
        );
    }
}

lines.push('');
lines.push('## Reading this report');
lines.push('');
lines.push('- Percentiles are computed from raw browser samples; warmups are excluded.');
lines.push(
    '- Compare only rows with matching workload, browser, record count, key/value size, batch size, and transaction boundaries.'
);
lines.push('- Do not treat native Criterion results as browser SDK/OPFS performance.');
lines.push('- Commit the raw JSON alongside any published report so claims remain reproducible.');

await writeFile(outPath, `${lines.join('\n')}\n`);
console.log(`wrote ${join('bench', 'results', 'report.md')}`);

function metric(value) {
    return typeof value === 'number' ? value.toFixed(2) : 'n/a';
}

function tableText(value = '') {
    return String(value).replace(/\|/g, '/').replace(/\s+/g, ' ').trim();
}
