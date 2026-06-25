import type { BenchReport, BenchResult, BenchStats } from './types';

export function computeStats(samples: number[]): BenchStats | undefined {
    if (samples.length === 0) {
        return undefined;
    }
    const sorted = [...samples].sort((a, b) => a - b);
    const sum = sorted.reduce((acc, value) => acc + value, 0);
    return {
        min: sorted[0],
        max: sorted[sorted.length - 1],
        mean: sum / sorted.length,
        p50: percentile(sorted, 0.5),
        p95: percentile(sorted, 0.95),
        p99: percentile(sorted, 0.99)
    };
}

function percentile(sorted: number[], p: number): number {
    if (sorted.length === 1) {
        return sorted[0];
    }
    const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * p) - 1));
    return sorted[index];
}

export function renderMarkdownReport(report: BenchReport): string {
    const lines: string[] = [];
    lines.push('# Browser benchmark report');
    lines.push('');
    lines.push(`Generated: ${report.generatedAt}`);
    lines.push(`Profile: ${report.profile}`);
    lines.push(`Browser: ${report.browser.name} ${report.browser.version}`.trim());
    lines.push(`User agent: ${report.browser.userAgent}`);
    lines.push('');
    lines.push('## Environment');
    lines.push('');
    lines.push(`- Timestamp: ${report.environment.timestamp}`);
    lines.push(`- Platform/OS: ${report.environment.os ?? 'unknown'}`);
    lines.push(`- Headless: ${String(report.environment.headless)}`);
    lines.push(`- WebDriver: ${String(report.environment.webdriver)}`);
    lines.push(`- SDK build mode: ${report.environment.sdkBuildMode}`);
    lines.push(`- WASM build mode: ${report.environment.wasmBuildMode}`);
    lines.push(`- Backend path: ${report.environment.backendPath}`);
    lines.push(`- Secure context: ${String(report.environment.secureContext)}`);
    lines.push(`- OPFS/getDirectory: ${String(report.environment.opfsSupported)}`);
    lines.push(`- SyncAccessHandle in dedicated Worker: ${String(report.environment.syncAccessHandleSupported)}`);
    lines.push(`- navigator.locks: ${String(report.environment.locksSupported)}`);
    lines.push(`- BroadcastChannel: ${String(report.environment.broadcastChannelSupported)}`);
    lines.push(`- Worker: ${String(report.environment.workerSupported)}`);
    lines.push(`- Persistent browser context: ${String(report.environment.persistentContext)}`);
    lines.push('');
    lines.push(
        'Native Rust engine microbench: not included in this browser report. Use `cargo bench` for native Criterion microbenchmarks.'
    );
    lines.push('WASM/Worker diagnostic benches: included for selected diagnostic workloads.');
    lines.push('Browser SDK bench: included for MoyoDB workloads marked `ok`.');
    lines.push('OPFS persistence bench: included for MoyoDB workloads that exercise open/recovery/snapshot paths.');
    lines.push('IndexedDB comparison bench: included for comparable workloads marked `ok`.');
    lines.push(
        'Worker transport overhead bench: included for `worker_roundtrip_overhead` or `noop_worker_roundtrip_10k` when selected.'
    );
    lines.push('');
    lines.push(
        'Results vary by browser, device, quota state, transaction boundaries, and warm/cold cache state. Do not publish a single best run as a project-wide claim.'
    );
    lines.push('');
    lines.push(
        '| Engine | Workload | Status | warmups | n | p50 ms | p95 ms | p99 ms | mean ms | min ms | max ms | Notes |'
    );
    lines.push(
        '| ------ | -------- | ------ | ------- | - | ------ | ------ | ------ | ------- | ------ | ------ | ----- |'
    );
    for (const result of report.results) {
        lines.push(renderResultRow(result));
    }
    lines.push('');
    lines.push('## Notes');
    for (const note of report.notes) {
        lines.push(`- ${note}`);
    }
    return `${lines.join('\n')}\n`;
}

function renderResultRow(result: BenchResult): string {
    const stats = result.stats;
    return [
        result.engine,
        result.workloadName,
        result.status,
        String(result.warmupSamples.length),
        String(result.rawSamples.length),
        formatMetric(stats?.p50),
        formatMetric(stats?.p95),
        formatMetric(stats?.p99),
        formatMetric(stats?.mean),
        formatMetric(stats?.min),
        formatMetric(stats?.max),
        escapeTable(result.error ? `${result.notes}; ${result.error}` : result.notes)
    ]
        .join(' | ')
        .replace(/^/, '| ')
        .replace(/$/, ' |');
}

function formatMetric(value: number | undefined): string {
    return value === undefined ? 'n/a' : value.toFixed(2);
}

function escapeTable(value: string): string {
    return value.replace(/\|/g, '/').replace(/\s+/g, ' ').trim();
}
