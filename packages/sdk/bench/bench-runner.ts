import { indexedDbBaseline, NotApplicableError as IndexedDbNotApplicableError } from './indexeddb-baseline';
import { moyoDbBaseline, NotApplicableError as MoyoDbNotApplicableError } from './moyodb-baseline';
import { computeStats, renderMarkdownReport } from './report';
import type {
    BenchEngine,
    BenchEnvironment,
    BenchOptions,
    BenchReport,
    BenchResult,
    BrowserInfo,
    SampleContext,
    WorkloadRunner,
    WorkloadSpec
} from './types';
import { selectWorkloads } from './workloads';

const RUNNERS: Record<BenchEngine, WorkloadRunner> = {
    moyodb: moyoDbBaseline,
    indexeddb: indexedDbBaseline
};

type ViteImportMeta = ImportMeta & { env?: { MODE?: string; DEV?: boolean; PROD?: boolean } };
type IndexedDbFactoryWithDatabases = IDBFactory & {
    databases?: () => Promise<Array<{ name?: string | null }>>;
};
type IterableFileSystemDirectoryHandle = FileSystemDirectoryHandle & {
    entries: () => AsyncIterable<[string, unknown]>;
};

export interface BenchmarkStorageCleanupResult {
    before?: StorageEstimate;
    after?: StorageEstimate;
    indexedDbDeleted: string[];
    opfsDeleted: string[];
    skipped: string[];
    errors: string[];
}

export const defaultBenchOptions: BenchOptions = {
    engines: ['moyodb', 'indexeddb'],
    profile: 'smoke',
    dbNamePrefix: 'moyodb-bench',
    persistentContext: false
};

export async function runBenchmarkSuite(options: Partial<BenchOptions> = {}): Promise<BenchReport> {
    const normalized: BenchOptions = {
        ...defaultBenchOptions,
        ...options,
        engines: options.engines ?? defaultBenchOptions.engines,
        dbNamePrefix: options.dbNamePrefix ?? defaultBenchOptions.dbNamePrefix,
        persistentContext: options.persistentContext ?? defaultBenchOptions.persistentContext
    };
    const generatedAt = new Date().toISOString();
    const environment = await detectBenchEnvironment(generatedAt, normalized.persistentContext ?? false);
    const browser = environment.browser;
    const workloads = selectWorkloads(normalized.profile, normalized.workloadNames);
    const results: BenchResult[] = [];

    for (const workload of workloads) {
        for (const engine of normalized.engines) {
            const runner = RUNNERS[engine];
            const warmupCount = normalized.warmupCountOverride ?? workload.warmupCount;
            const sampleCount = normalized.sampleCountOverride ?? workload.sampleCount;
            results.push(
                await runOneWorkload(
                    runner,
                    workload,
                    browser,
                    generatedAt,
                    warmupCount,
                    sampleCount,
                    normalized.dbNamePrefix ?? 'moyodb-bench',
                    normalized.workloadTimeoutMs
                )
            );
        }
    }

    return {
        schemaVersion: 1,
        project: 'MoyoDB',
        generatedAt,
        profile: normalized.profile,
        browser,
        environment,
        results,
        notes: [
            'Raw samples are wall-clock milliseconds measured with performance.now() inside the browser page.',
            'Warmup samples are recorded separately and excluded from percentiles.',
            'Setup/preload/data generation/open/delete are outside the timed region unless the workload name explicitly says open/init or the notes say otherwise.',
            'MoyoDB browser measurements include SDK, Worker, WASM, and OPFS overhead unless a diagnostic workload isolates a lower layer.',
            'Native Rust microbenchmarks measure the engine core only. Browser benchmarks measure SDK/WASM/Worker/OPFS overhead. Do not compare them as if they measure the same path.',
            'IndexedDB is measured with the same record counts, key/value sizes, batch sizes, and transaction boundaries for comparable workloads.'
        ]
    };
}

export async function clearBenchmarkStorage(
    prefixes: string[] = ['moyodb-bench', 'playwright']
): Promise<BenchmarkStorageCleanupResult> {
    const result: BenchmarkStorageCleanupResult = {
        before: await estimateStorage(),
        after: undefined,
        indexedDbDeleted: [],
        opfsDeleted: [],
        skipped: [],
        errors: []
    };
    const normalizedPrefixes = prefixes.filter((prefix) => prefix.length > 0);

    await clearBenchmarkIndexedDb(normalizedPrefixes, result);
    await clearBenchmarkOpfs(normalizedPrefixes, result);

    result.after = await estimateStorage();
    return result;
}

async function runOneWorkload(
    runner: WorkloadRunner,
    workload: WorkloadSpec,
    browser: BrowserInfo,
    timestamp: string,
    warmupCount: number,
    sampleCount: number,
    dbNamePrefix: string,
    workloadTimeoutMs?: number
): Promise<BenchResult> {
    const warmupSamples: number[] = [];
    const rawSamples: number[] = [];
    const base: Omit<BenchResult, 'status' | 'warmupSamples' | 'rawSamples'> = {
        engine: runner.engine,
        workloadName: workload.name,
        browser,
        timestamp,
        recordCount: workload.recordCount,
        keySize: workload.keySize,
        valueSize: workload.valueSize,
        batchSize: workload.batchSize,
        transactionBoundaries: workload.transactionBoundaries,
        warmupCount,
        sampleCount,
        notes: workload.notes
    };

    if (!workload.supports.includes(runner.engine)) {
        console.info(`[bench] skip ${runner.engine}/${workload.name}`);
        return {
            ...base,
            status: 'skipped',
            warmupSamples,
            rawSamples,
            notes: `${workload.notes} Not applicable to ${runner.engine}.`
        };
    }

    try {
        console.info(`[bench] start ${runner.engine}/${workload.name} warmups=${warmupCount} samples=${sampleCount}`);
        for (let i = 0; i < warmupCount; i += 1) {
            const elapsed = await runPreparedSample(
                runner,
                {
                    engine: runner.engine,
                    dbName: `${dbNamePrefix}-${runner.engine}-${workload.name}-warmup-${i}-${Date.now()}`,
                    workload,
                    sampleIndex: i
                },
                workloadTimeoutMs
            );
            warmupSamples.push(elapsed);
            console.info(
                `[bench] warmup ${runner.engine}/${workload.name} ${i + 1}/${warmupCount}: ${elapsed.toFixed(2)}ms`
            );
            await yieldToBrowser();
        }
        for (let i = 0; i < sampleCount; i += 1) {
            const elapsed = await runPreparedSample(
                runner,
                {
                    engine: runner.engine,
                    dbName: `${dbNamePrefix}-${runner.engine}-${workload.name}-sample-${i}-${Date.now()}`,
                    workload,
                    sampleIndex: i
                },
                workloadTimeoutMs
            );
            rawSamples.push(elapsed);
            console.info(
                `[bench] sample ${runner.engine}/${workload.name} ${i + 1}/${sampleCount}: ${elapsed.toFixed(2)}ms`
            );
            await yieldToBrowser();
        }
        console.info(`[bench] done ${runner.engine}/${workload.name}`);
        return {
            ...base,
            status: 'ok',
            warmupSamples,
            rawSamples,
            stats: computeStats(rawSamples)
        };
    } catch (error) {
        if (error instanceof IndexedDbNotApplicableError || error instanceof MoyoDbNotApplicableError) {
            console.info(`[bench] skip ${runner.engine}/${workload.name}: ${error.message}`);
            return {
                ...base,
                status: 'skipped',
                warmupSamples,
                rawSamples,
                notes: `${workload.notes} ${error.message}`
            };
        }
        console.info(
            `[bench] error ${runner.engine}/${workload.name}: ${error instanceof Error ? error.message : String(error)}`
        );
        return {
            ...base,
            status: 'error',
            warmupSamples,
            rawSamples,
            error: error instanceof Error ? `${error.name}: ${error.message}` : String(error)
        };
    }
}

async function detectBenchEnvironment(timestamp: string, persistentContext: boolean): Promise<BenchEnvironment> {
    const browser = detectBrowserInfo();
    const nav = navigator as Navigator & {
        locks?: unknown;
        storage?: StorageManager & { getDirectory?: unknown };
    };
    const importMeta = import.meta as ViteImportMeta;
    const opfsSupported = typeof nav.storage?.getDirectory === 'function';
    const syncAccessHandleSupported = opfsSupported ? await hasSyncAccessHandle() : false;
    const sdkMode =
        importMeta.env?.MODE ?? (importMeta.env?.PROD ? 'production' : importMeta.env?.DEV ? 'development' : 'unknown');
    return {
        browser,
        timestamp,
        headless: /Headless/i.test(browser.userAgent) ? true : 'unknown',
        os: browser.platform,
        secureContext: globalThis.isSecureContext,
        webdriver: Boolean(nav.webdriver),
        sdkBuildMode: sdkMode,
        wasmBuildMode:
            'not introspected at runtime; use npm run build:wasm:release before publishing benchmark numbers',
        backendPath: syncAccessHandleSupported ? 'OPFS SyncAccessHandle in a dedicated Worker' : 'unavailable',
        opfsSupported,
        syncAccessHandleSupported,
        locksSupported: Boolean(nav.locks),
        broadcastChannelSupported: typeof globalThis.BroadcastChannel !== 'undefined',
        workerSupported: typeof globalThis.Worker === 'function',
        persistentContext
    };
}

function detectBrowserInfo(): BrowserInfo {
    const nav = navigator as Navigator & {
        userAgentData?: {
            platform?: string;
            brands?: Array<{ brand: string; version: string }>;
        };
    };
    const ua = nav.userAgent ?? 'unknown';
    const brand = nav.userAgentData?.brands?.find((item) => !/Not.?A.?Brand/i.test(item.brand));
    const parsed = parseUserAgent(ua);
    return {
        name: brand?.brand ?? parsed.name,
        version: brand?.version ?? parsed.version,
        userAgent: ua,
        platform: nav.userAgentData?.platform ?? nav.platform
    };
}

async function estimateStorage(): Promise<StorageEstimate | undefined> {
    try {
        return await navigator.storage?.estimate?.();
    } catch {
        return undefined;
    }
}

async function clearBenchmarkIndexedDb(prefixes: string[], result: BenchmarkStorageCleanupResult): Promise<void> {
    const idb = indexedDB as IndexedDbFactoryWithDatabases;
    if (typeof idb.databases !== 'function') {
        result.skipped.push('IndexedDB database enumeration is unavailable');
        return;
    }
    let databases: Array<{ name?: string | null }>;
    try {
        databases = await idb.databases();
    } catch (error) {
        result.errors.push(`IndexedDB enumeration failed: ${formatError(error)}`);
        return;
    }
    for (const { name } of databases) {
        if (!name || !isBenchmarkName(name, prefixes)) {
            continue;
        }
        const deleted = await deleteIndexedDb(name);
        if (deleted) {
            result.indexedDbDeleted.push(name);
        } else {
            result.errors.push(`IndexedDB delete was blocked or failed: ${name}`);
        }
    }
}

function deleteIndexedDb(name: string): Promise<boolean> {
    return new Promise((resolve) => {
        const request = indexedDB.deleteDatabase(name);
        request.onsuccess = () => resolve(true);
        request.onerror = () => resolve(false);
        request.onblocked = () => resolve(false);
    });
}

async function clearBenchmarkOpfs(prefixes: string[], result: BenchmarkStorageCleanupResult): Promise<void> {
    const storage = navigator.storage as StorageManager & {
        getDirectory?: () => Promise<FileSystemDirectoryHandle>;
    };
    if (typeof storage.getDirectory !== 'function') {
        result.skipped.push('OPFS directory API is unavailable');
        return;
    }

    let root: FileSystemDirectoryHandle;
    try {
        root = await storage.getDirectory();
    } catch (error) {
        result.errors.push(`OPFS root open failed: ${formatError(error)}`);
        return;
    }

    await removeOpfsRootIfExists(root, '__moyodb_bench_raw__', result);
    await removeOpfsRootIfExists(root, '__moyodb_bench_support__', result);
    await removeOpfsRootIfExists(root, '__moyodb_capability__', result);

    let stackdb: IterableFileSystemDirectoryHandle | null = null;
    try {
        stackdb = (await root.getDirectoryHandle('stackdb', { create: false })) as IterableFileSystemDirectoryHandle;
    } catch (error) {
        if (!(error instanceof DOMException && error.name === 'NotFoundError')) {
            result.errors.push(`OPFS stackdb open failed: ${formatError(error)}`);
        }
    }
    if (!stackdb) {
        return;
    }

    for await (const [entryName] of stackdb.entries()) {
        const decoded = decodeHexName(entryName);
        if (!decoded || !isBenchmarkName(decoded, prefixes)) {
            continue;
        }
        try {
            await stackdb.removeEntry(entryName, { recursive: true });
            result.opfsDeleted.push(`stackdb/${decoded}`);
        } catch (error) {
            result.errors.push(`OPFS delete failed for stackdb/${decoded}: ${formatError(error)}`);
        }
    }
}

async function removeOpfsRootIfExists(
    root: FileSystemDirectoryHandle,
    name: string,
    result: BenchmarkStorageCleanupResult
): Promise<void> {
    try {
        await root.removeEntry(name, { recursive: true });
        result.opfsDeleted.push(name);
    } catch (error) {
        if (!(error instanceof DOMException && error.name === 'NotFoundError')) {
            result.errors.push(`OPFS delete failed for ${name}: ${formatError(error)}`);
        }
    }
}

function isBenchmarkName(name: string, prefixes: string[]): boolean {
    return (
        prefixes.some((prefix) => name === prefix || name.startsWith(`${prefix}-`)) ||
        name.startsWith('__moyodb_idb_probe__')
    );
}

function decodeHexName(value: string): string | null {
    if (value.length % 2 !== 0 || /[^0-9a-f]/i.test(value)) {
        return null;
    }
    const bytes = new Uint8Array(value.length / 2);
    for (let i = 0; i < bytes.length; i += 1) {
        bytes[i] = Number.parseInt(value.slice(i * 2, i * 2 + 2), 16);
    }
    try {
        return new TextDecoder().decode(bytes);
    } catch {
        return null;
    }
}

function formatError(error: unknown): string {
    return error instanceof Error ? `${error.name}: ${error.message}` : String(error);
}

function parseUserAgent(ua: string): { name: string; version: string } {
    const patterns: Array<[string, RegExp]> = [
        ['Firefox', /Firefox\/(\d+(?:\.\d+)?)/],
        ['Chromium', /Chrom(?:e|ium)\/(\d+(?:\.\d+)?)/],
        ['Safari', /Version\/(\d+(?:\.\d+)?).*Safari\//]
    ];
    for (const [name, pattern] of patterns) {
        const match = pattern.exec(ua);
        if (match) {
            return { name, version: match[1] };
        }
    }
    return { name: 'unknown', version: 'unknown' };
}

async function hasSyncAccessHandle(): Promise<boolean> {
    try {
        const source = `
self.onmessage = async () => {
  try {
    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle('__moyodb_bench_support__', { create: true });
    const file = await dir.getFileHandle('probe.bin', { create: true });
    if (typeof file.createSyncAccessHandle !== 'function') {
      self.postMessage(false);
      return;
    }
    const handle = await file.createSyncAccessHandle();
    handle.close();
    self.postMessage(true);
  } catch {
    self.postMessage(false);
  }
};`;
        const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
        const worker = new Worker(url, { type: 'module' });
        try {
            return await new Promise<boolean>((resolve) => {
                const timeout = setTimeout(() => resolve(false), 3000);
                worker.onmessage = (event) => {
                    clearTimeout(timeout);
                    resolve(event.data === true);
                };
                worker.onerror = () => {
                    clearTimeout(timeout);
                    resolve(false);
                };
                worker.postMessage(null);
            });
        } finally {
            worker.terminate();
            URL.revokeObjectURL(url);
        }
    } catch {
        return false;
    }
}

function yieldToBrowser(): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, 0));
}

async function runPreparedSample(
    runner: WorkloadRunner,
    ctx: SampleContext,
    timeoutMs: number | undefined
): Promise<number> {
    const cleanup = await withOptionalTimeout(
        Promise.resolve(runner.prepare?.(ctx)),
        timeoutMs,
        `${runner.engine}/${ctx.workload.name} setup timed out after ${timeoutMs}ms`
    );
    try {
        const started = performance.now();
        await withOptionalTimeout(
            runner.run(ctx),
            timeoutMs,
            `${runner.engine}/${ctx.workload.name} sample timed out after ${timeoutMs}ms`
        );
        return performance.now() - started;
    } finally {
        await cleanup?.();
        await runner.cleanup?.(ctx);
    }
}

function withOptionalTimeout<T>(promise: Promise<T>, timeoutMs: number | undefined, message: string): Promise<T> {
    if (timeoutMs === undefined || timeoutMs <= 0) {
        return promise;
    }
    return new Promise<T>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error(message)), timeoutMs);
        promise.then(
            (value) => {
                clearTimeout(timeout);
                resolve(value);
            },
            (error) => {
                clearTimeout(timeout);
                reject(error);
            }
        );
    });
}

export function serializeReport(report: BenchReport): string {
    return JSON.stringify(report, null, 2);
}

export function reportToMarkdown(report: BenchReport): string {
    return renderMarkdownReport(report);
}

declare global {
    interface Window {
        moyodbBench?: {
            runBenchmarkSuite: typeof runBenchmarkSuite;
            clearBenchmarkStorage: typeof clearBenchmarkStorage;
            serializeReport: typeof serializeReport;
            reportToMarkdown: typeof reportToMarkdown;
            defaultBenchOptions: BenchOptions;
        };
    }
}

if (typeof window !== 'undefined') {
    window.moyodbBench = {
        runBenchmarkSuite,
        clearBenchmarkStorage,
        serializeReport,
        reportToMarkdown,
        defaultBenchOptions
    };
}
