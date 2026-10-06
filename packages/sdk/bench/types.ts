export type BenchEngine = 'moyodb' | 'indexeddb';
export type BenchStatus = 'ok' | 'skipped' | 'error';
export type BenchProfile = 'smoke' | 'standard' | 'full';
export type DatasetProfile =
    'lcg-repeat-256' | 'high-entropy-binary' | 'realistic-json' | 'realistic-text' | 'precompressed';
export type BenchCompression = false | 'snappy' | 'gzip' | 'deflate';

export interface BenchPolicy {
    dataset: { profile: DatasetProfile; version: 1; seed: number };
    /** MoyoDB store codec. IndexedDB writes the same input bytes without this codec. */
    compression: BenchCompression;
    compressionTuning: {
        algorithm: 'snappy-raw-block';
        algorithmVersion: number;
        thresholdBytes: number;
        minimumSavingPercent: number;
        envelopeBytes: number;
        sampling: {
            minimumInputBytes: number;
            windowBytes: number;
            windowCount: number;
            positioning: 'start-middle-end';
            profitability: 'aggregate-size-plus-one-envelope';
        };
    };
    changeFeed: { enabled: boolean; retainTxids: number | null };
    timing: 'run-callback-only-v1';
}

export interface BenchSampleMetrics {
    explicitCommitMs: number | null;
    explicitCommitCount: number;
    verificationMs: number;
    cleanupMs: number;
    closeMs: number | null;
    logicalValueBytes: number;
    databaseBytes: number | null;
    manifestBytes: number | null;
    mainBytes: number | null;
    walBytes: number | null;
    backendReads: number | null;
    backendWrites: number | null;
    backendFlushes: number | null;
}

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
    policy?: BenchPolicy;
}

export interface SampleContext {
    dbName: string;
    workload: WorkloadSpec;
    sampleIndex: number;
    engine: BenchEngine;
    indexedDbDurability: IDBTransactionDurability;
}

export interface WorkloadRunner {
    engine: BenchEngine;
    prepare?(ctx: SampleContext): Promise<(() => Promise<void>) | void>;
    run(ctx: SampleContext): Promise<void>;
    /**
     * Runs after timing, before cleanup. Throws if sample data differs from the
     * deterministic dataset. Returns a checksum that must match across engines,
     * or null when the workload has no comparable content.
     */
    verify?(ctx: SampleContext): Promise<string | null>;
    cleanup?(ctx: SampleContext): Promise<void>;
    metrics?(ctx: SampleContext): Promise<Partial<BenchSampleMetrics>>;
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
    gitSha?: string;
    indexedDbDurability?: IDBTransactionDurability;
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
    gitSha: string;
    sdkBuildMode: string;
    wasmBuildMode: string;
    backendPath: string;
    indexedDbDurability: IDBTransactionDurability;
    moyoDbDurability: string;
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
    policy: BenchPolicy;
    warmupCount: number;
    sampleCount: number;
    warmupSamples: number[];
    rawSamples: number[];
    /** One entry per measured sample, aligned with `rawSamples`. */
    contentChecksums: Array<string | null>;
    sampleMetrics: BenchSampleMetrics[];
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
