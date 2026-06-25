export type BenchEngine = 'moyodb' | 'indexeddb';
export type BenchStatus = 'ok' | 'skipped' | 'error';
export type BenchProfile = 'smoke' | 'standard' | 'full';

export interface WorkloadSpec {
    name: string;
    recordCount: number;
    keySize: number;
    valueSize: number;
    batchSize: number;
    transactionBoundaries: string;
    warmupCount: number;
    sampleCount: number;
    notes: string;
    tags?: string[];
    smoke?: boolean;
    supports: BenchEngine[];
}

export interface SampleContext {
    dbName: string;
    workload: WorkloadSpec;
    sampleIndex: number;
    engine: BenchEngine;
}

export interface WorkloadRunner {
    engine: BenchEngine;
    prepare?(ctx: SampleContext): Promise<(() => Promise<void>) | void>;
    run(ctx: SampleContext): Promise<void>;
    cleanup?(ctx: SampleContext): Promise<void>;
}

export interface BenchOptions {
    engines: BenchEngine[];
    profile: BenchProfile;
    workloadNames?: string[];
    sampleCountOverride?: number;
    warmupCountOverride?: number;
    dbNamePrefix?: string;
    workloadTimeoutMs?: number;
    persistentContext?: boolean;
}

export interface BrowserInfo {
    name: string;
    version: string;
    userAgent: string;
    platform?: string;
}

export interface BenchEnvironment {
    browser: BrowserInfo;
    timestamp: string;
    headless: boolean | 'unknown';
    os?: string;
    secureContext: boolean;
    webdriver: boolean;
    sdkBuildMode: string;
    wasmBuildMode: string;
    backendPath: string;
    opfsSupported: boolean;
    syncAccessHandleSupported: boolean;
    locksSupported: boolean;
    broadcastChannelSupported: boolean;
    workerSupported: boolean;
    persistentContext: boolean;
}

export interface BenchStats {
    min: number;
    max: number;
    mean: number;
    p50: number;
    p95: number;
    p99: number;
}

export interface BenchResult {
    status: BenchStatus;
    engine: BenchEngine;
    workloadName: string;
    browser: BrowserInfo;
    timestamp: string;
    recordCount: number;
    keySize: number;
    valueSize: number;
    batchSize: number;
    transactionBoundaries: string;
    warmupCount: number;
    sampleCount: number;
    warmupSamples: number[];
    rawSamples: number[];
    stats?: BenchStats;
    notes: string;
    error?: string;
}

export interface BenchReport {
    schemaVersion: 1;
    project: 'MoyoDB';
    generatedAt: string;
    profile: BenchProfile;
    browser: BrowserInfo;
    environment: BenchEnvironment;
    results: BenchResult[];
    notes: string[];
}
