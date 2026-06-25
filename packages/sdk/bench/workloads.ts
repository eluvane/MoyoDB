import type { BenchProfile, WorkloadSpec } from './types';

export const STORE_NAME = 'kv';
export const OPFS_DIAGNOSTIC_BYTES = 100 * 1024 * 1024;

export const WORKLOADS: WorkloadSpec[] = [
    workload(
        'noop_js_loop_1m',
        1_000_000,
        0,
        0,
        0,
        'pure JavaScript loop; no storage',
        1,
        5,
        'Diagnostic: isolates JS loop overhead.',
        true,
        ['moyodb'],
        ['diagnostic']
    ),
    workload(
        'noop_worker_roundtrip_10k',
        10_000,
        0,
        0,
        1,
        '10,000 sequential postMessage echo roundtrips',
        1,
        5,
        'Diagnostic: legacy raw Worker echo latency without SDK, WASM, or OPFS.',
        true,
        ['moyodb'],
        ['diagnostic', 'worker']
    ),
    workload(
        'worker_roundtrip_noop',
        10_000,
        0,
        0,
        1,
        '10,000 sequential protocol-shaped Worker roundtrips with no payload',
        1,
        5,
        'Diagnostic: isolates Worker request/response latency without SDK, WASM, OPFS, or data generation.',
        true,
        ['moyodb'],
        ['diagnostic', 'worker', 'ipc']
    ),
    workload(
        'worker_roundtrip_small_payload',
        10_000,
        0,
        32,
        1,
        '10,000 sequential Worker roundtrips with a 32-byte Uint8Array payload',
        1,
        5,
        'Diagnostic: isolates structured clone overhead for a tiny binary payload.',
        true,
        ['moyodb'],
        ['diagnostic', 'worker', 'ipc']
    ),
    workload(
        'worker_roundtrip_256b_payload',
        10_000,
        0,
        256,
        1,
        '10,000 sequential Worker roundtrips with a 256-byte Uint8Array payload',
        1,
        5,
        'Diagnostic: isolates structured clone overhead for a representative small value.',
        true,
        ['moyodb'],
        ['diagnostic', 'worker', 'ipc']
    ),
    workload(
        'worker_roundtrip_64kb_payload',
        100,
        0,
        64 * 1024,
        1,
        '100 sequential Worker roundtrips with a 64 KiB Uint8Array payload using structured clone',
        1,
        5,
        'Diagnostic: isolates large binary structured clone overhead.',
        true,
        ['moyodb'],
        ['diagnostic', 'worker', 'ipc']
    ),
    workload(
        'worker_binary_transfer_64kb',
        100,
        0,
        64 * 1024,
        1,
        '100 sequential Worker roundtrips with a 64 KiB Uint8Array payload transferred both directions',
        1,
        5,
        'Diagnostic: isolates transferable ArrayBuffer roundtrip overhead; buffers are generated during setup and ownership is intentionally moved.',
        true,
        ['moyodb'],
        ['diagnostic', 'worker', 'ipc']
    ),
    workload(
        'noop_wasm_call_100k',
        100_000,
        0,
        0,
        1,
        '100,000 get_schema_version calls inside a worker-held WASM engine',
        0,
        3,
        'Diagnostic: isolates repeated JS-to-WASM method dispatch after OPFS-backed engine setup.',
        true,
        ['moyodb'],
        ['diagnostic', 'wasm']
    ),
    workload(
        'encode_decode_10k_256b',
        10_000,
        16,
        256,
        1_000,
        'encode/decode 10,000 deterministic 256-byte values',
        1,
        5,
        'Diagnostic: isolates benchmark key/value generation and byte-copy cost.',
        true,
        ['moyodb'],
        ['diagnostic', 'codec']
    ),
    workload(
        'opfs_raw_write_100mb',
        OPFS_DIAGNOSTIC_BYTES,
        0,
        1024 * 1024,
        1024 * 1024,
        'write 100 MiB through one worker-held SyncAccessHandle, one flush at end',
        0,
        3,
        'Diagnostic: OPFS raw sequential write throughput; no SDK, no WASM engine.',
        false,
        ['moyodb'],
        ['diagnostic', 'opfs']
    ),
    workload(
        'opfs_raw_read_random_10k',
        10_000,
        0,
        4096,
        4096,
        'setup writes 100 MiB; measurement performs 10,000 random 4 KiB SyncAccessHandle reads',
        0,
        3,
        'Diagnostic: OPFS raw random-read cost; setup is outside the timed region.',
        false,
        ['moyodb'],
        ['diagnostic', 'opfs']
    ),
    workload(
        'sdk_put_1k_single_calls',
        1_000,
        16,
        256,
        1,
        '1,000 db.put calls; each call opens/scopes/commits one SDK transaction',
        0,
        1,
        'Smoke diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path.',
        true,
        ['moyodb'],
        ['diagnostic', 'sdk']
    ),
    workload(
        'sdk_put_10k_single_calls',
        10_000,
        16,
        256,
        1,
        '10,000 db.put calls; each call opens/scopes/commits one SDK transaction',
        0,
        1,
        'Heavy diagnostic: public SDK single-call overhead. This is intentionally not a bulk insert path and is excluded from the default smoke profile.',
        false,
        ['moyodb'],
        ['diagnostic', 'sdk']
    ),
    workload(
        'sdk_bulk_put_10k',
        10_000,
        16,
        256,
        10_000,
        'one SDK transaction, one tx.putMany call with 10,000 entries, one commit',
        1,
        5,
        'Diagnostic: SDK bulk put path after data generation and empty DB setup.',
        true,
        ['moyodb'],
        ['diagnostic', 'sdk']
    ),
    workload(
        'engine_stage_put_10k_rollback',
        10_000,
        16,
        256,
        10_000,
        'one worker-local WASM put_many call with 10,000 entries followed by rollback; no commit flush',
        0,
        3,
        'Diagnostic: isolates WASM conversion and in-memory transaction staging without BTree commit or OPFS flush.',
        true,
        ['moyodb'],
        ['diagnostic', 'engine']
    ),
    workload(
        'engine_bulk_put_10k',
        10_000,
        16,
        256,
        10_000,
        'one worker-local WASM engine transaction, one put_many call with 10,000 entries, one commit',
        0,
        3,
        'Diagnostic: bypasses public SDK payload transfer; isolates worker/WASM/OPFS engine bulk path.',
        true,
        ['moyodb'],
        ['diagnostic', 'engine']
    ),
    workload(
        'indexeddb_bulk_put_10k',
        10_000,
        16,
        256,
        10_000,
        'one IndexedDB readwrite transaction with 10,000 puts',
        1,
        5,
        'Diagnostic: IndexedDB bulk baseline with setup and data generation outside the timed region.',
        true,
        ['indexeddb'],
        ['diagnostic', 'indexeddb']
    ),

    workload(
        'open_empty_db',
        1,
        0,
        0,
        1,
        'database deletion/setup is outside measurement; timed region opens an empty DB and creates no store',
        1,
        5,
        'Open/init diagnostic. MoyoDB includes Worker, WASM module initialization, and OPFS open when no cached worker exists.',
        true,
        ['moyodb', 'indexeddb'],
        ['open']
    ),

    workload(
        'bulk_insert_10k',
        10_000,
        16,
        256,
        1_000,
        'one readwrite transaction per batch of 1,000 puts',
        1,
        5,
        'Comparable batch insert workload. Test data and empty DB setup are outside the measured region.',
        true,
        ['moyodb', 'indexeddb'],
        ['write']
    ),
    workload(
        'bulk_insert_100k',
        100_000,
        16,
        256,
        1_000,
        'one readwrite transaction per batch of 1,000 puts',
        1,
        5,
        'Longer batch insert workload. Test data and empty DB setup are outside the measured region.',
        false,
        ['moyodb', 'indexeddb'],
        ['write']
    ),
    workload(
        'bulk_insert_1m',
        1_000_000,
        16,
        256,
        5_000,
        'one readwrite transaction per batch of 5,000 puts',
        0,
        3,
        'Heavy launch benchmark. Test data and empty DB setup are outside the measured region.',
        false,
        ['moyodb', 'indexeddb'],
        ['write', 'full']
    ),
    workload(
        'bulk_insert_1m_batched_1000',
        1_000_000,
        16,
        256,
        1_000,
        'one readwrite transaction per batch of 1,000 puts',
        0,
        3,
        '1M insert with smaller commit batches. Test data and empty DB setup are outside the measured region.',
        false,
        ['moyodb', 'indexeddb'],
        ['write', 'full']
    ),
    workload(
        'bulk_insert_1m_batched_10000',
        1_000_000,
        16,
        256,
        10_000,
        'one readwrite transaction per batch of 10,000 puts',
        0,
        3,
        '1M insert with larger commit batches. Test data and empty DB setup are outside the measured region.',
        false,
        ['moyodb', 'indexeddb'],
        ['write', 'full']
    ),
    workload(
        'bulk_insert_1m_single_tx',
        1_000_000,
        16,
        256,
        10_000,
        'one readwrite transaction; putMany chunks of 10,000; one commit',
        0,
        1,
        'Pathological large single-transaction probe. Data generation/open/delete are outside measurement; do not use as the headline browser result without the batched rows.',
        false,
        ['moyodb', 'indexeddb'],
        ['write', 'full', 'pathological']
    ),
    workload(
        'cold_insert_1m_single_tx',
        1_000_000,
        16,
        256,
        10_000,
        'compatibility alias for bulk_insert_1m_single_tx; one readwrite transaction, putMany chunks of 10,000, one commit',
        0,
        1,
        'Deprecated compatibility row for old reports. The measured region no longer includes data generation, open, or cleanup.',
        false,
        ['moyodb'],
        ['write', 'full', 'pathological']
    ),

    workload(
        'point_get_random_10k',
        10_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; measured region performs 10,000 single tx.get calls inside one readonly transaction',
        1,
        5,
        'Random point reads after a 10k-row setup. This measures per-call SDK/Worker/WASM overhead.',
        true,
        ['moyodb', 'indexeddb'],
        ['read']
    ),
    workload(
        'point_get_random_10k_bulk',
        10_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; measured region performs one getMany over 10,000 random keys',
        1,
        5,
        'Bulk random point reads after a 10k-row setup. This measures the recommended public bulk read path.',
        true,
        ['moyodb', 'indexeddb'],
        ['read', 'bulk']
    ),
    workload(
        'point_get_random_100k',
        100_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; measured region performs 10,000 single tx.get calls inside one readonly transaction',
        1,
        5,
        'Random point reads after a 100k-row setup. This measures per-call SDK/Worker/WASM overhead.',
        false,
        ['moyodb', 'indexeddb'],
        ['read']
    ),
    workload(
        'point_get_random_1m',
        1_000_000,
        16,
        256,
        5_000,
        'setup/preload outside measurement; measured region performs 10,000 single tx.get calls inside one readonly transaction',
        0,
        3,
        'Heavy random-read benchmark after 1M-row setup. Preload is not included in raw samples.',
        false,
        ['moyodb', 'indexeddb'],
        ['read', 'full']
    ),
    workload(
        'point_get_random_1m_preloaded',
        1_000_000,
        16,
        256,
        5_000,
        'compatibility alias: setup/preload outside measurement; measured region performs 10,000 single tx.get calls',
        0,
        1,
        'Deprecated compatibility row for old reports. Preload, key generation, open, and cleanup are outside the timed region.',
        false,
        ['moyodb'],
        ['read', 'full']
    ),
    workload(
        'random_get_10k_from_1m',
        1_000_000,
        16,
        256,
        5_000,
        'setup/preload outside measurement; measured region performs 10,000 single random gets',
        0,
        3,
        'Layered read workload: 1M-row preload outside timed region, 10k individual SDK gets measured.',
        false,
        ['moyodb', 'indexeddb'],
        ['read', 'full']
    ),
    workload(
        'random_get_10k_from_1m_bulk',
        1_000_000,
        16,
        256,
        5_000,
        'setup/preload outside measurement; measured region performs one getMany over 10,000 random keys',
        0,
        3,
        'Layered read workload: same keys as single-get row, but one bulk read call to isolate Worker roundtrip overhead.',
        false,
        ['moyodb', 'indexeddb'],
        ['read', 'bulk', 'full']
    ),

    workload(
        'range_scan_100',
        10_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; measurement scans 100 contiguous keys',
        1,
        5,
        'Range scan over 100 rows.',
        true,
        ['moyodb', 'indexeddb'],
        ['scan']
    ),
    workload(
        'range_scan_1000',
        100_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; measurement scans 1,000 contiguous keys',
        1,
        5,
        'Range scan over 1,000 rows.',
        false,
        ['moyodb', 'indexeddb'],
        ['scan']
    ),
    workload(
        'range_scan_10000',
        100_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; measurement scans 10,000 contiguous keys',
        1,
        3,
        'Range scan over 10,000 rows.',
        false,
        ['moyodb', 'indexeddb'],
        ['scan']
    ),
    workload(
        'range_scan_1000_from_1m',
        1_000_000,
        16,
        256,
        5_000,
        'setup/preload outside measurement; measurement scans 1,000 contiguous keys from a 1M-row DB',
        0,
        3,
        'Layered scan workload with 1M-row preload outside timed region.',
        false,
        ['moyodb', 'indexeddb'],
        ['scan', 'full']
    ),

    workload(
        'small_tx_1000_commits',
        1_000,
        16,
        128,
        1,
        '1,000 independent readwrite commits; keys/values and empty DB setup outside measurement',
        1,
        5,
        'Small transaction commit overhead.',
        true,
        ['moyodb', 'indexeddb'],
        ['write']
    ),
    workload(
        'batch_tx_100k_values_256b',
        100_000,
        16,
        256,
        5_000,
        '20 readwrite transactions of 5,000 puts each; data/setup outside measurement',
        1,
        5,
        'Large batch write with 256-byte values.',
        false,
        ['moyodb', 'indexeddb'],
        ['write']
    ),

    workload(
        'large_value_64kb',
        1_000,
        16,
        64 * 1024,
        100,
        'one readwrite transaction per 100 large values; data/setup outside measurement',
        1,
        5,
        'Large values that exercise overflow/page paths.',
        false,
        ['moyodb', 'indexeddb'],
        ['write']
    ),
    workload(
        'large_value_1mb',
        64,
        16,
        1024 * 1024,
        8,
        'one readwrite transaction per 8 large values; data/setup outside measurement',
        1,
        3,
        'Very large values; disabled in smoke profile.',
        false,
        ['moyodb', 'indexeddb'],
        ['write']
    ),

    workload(
        'cold_open_after_100k',
        100_000,
        16,
        256,
        5_000,
        'setup/preload/close outside measurement; timed region opens populated database and performs one verification read',
        1,
        5,
        'Cold open measurement after a 100k-row setup.',
        false,
        ['moyodb', 'indexeddb'],
        ['open']
    ),
    workload(
        'recovery_after_dirty_close',
        10_000,
        16,
        256,
        1_000,
        'MoyoDB debug failpoint after WAL flush; IndexedDB baseline is marked not applicable',
        1,
        5,
        'Measures recovery after a simulated dirty close using an engine failpoint.',
        false,
        ['moyodb'],
        ['recovery']
    ),
    workload(
        'snapshot_export_import',
        10_000,
        16,
        256,
        1_000,
        'setup is outside measurement; measurement exports and imports a logical snapshot',
        1,
        5,
        'MoyoDB snapshot roundtrip; IndexedDB baseline is marked not applicable.',
        false,
        ['moyodb'],
        ['snapshot']
    ),
    workload(
        'worker_roundtrip_overhead',
        1_000,
        16,
        64,
        1,
        'measurement calls stats() 1,000 times through the SDK worker boundary',
        1,
        5,
        'MoyoDB SDK worker transport overhead; IndexedDB baseline is marked not applicable.',
        true,
        ['moyodb'],
        ['diagnostic', 'worker']
    )
];

function workload(
    name: string,
    recordCount: number,
    keySize: number,
    valueSize: number,
    batchSize: number,
    transactionBoundaries: string,
    warmupCount: number,
    sampleCount: number,
    notes: string,
    smoke: boolean,
    supports: WorkloadSpec['supports'],
    tags: string[] = []
): WorkloadSpec {
    return {
        name,
        recordCount,
        keySize,
        valueSize,
        batchSize,
        transactionBoundaries,
        warmupCount,
        sampleCount,
        notes,
        smoke,
        supports,
        tags
    };
}

export function selectWorkloads(profile: BenchProfile, workloadNames?: string[]): WorkloadSpec[] {
    const requested = workloadNames?.length ? new Set(workloadNames) : null;
    if (requested) {
        return WORKLOADS.filter((workload) => requested.has(workload.name));
    }
    return WORKLOADS.filter((workload) => {
        if (profile === 'smoke') {
            return workload.smoke === true;
        }
        if (profile === 'standard') {
            return !workload.tags?.includes('full') && workload.name !== 'large_value_1mb';
        }
        return true;
    });
}

export function keyString(index: number, keySize: number): string {
    if (keySize <= 0) {
        return '';
    }
    const numeric = Math.max(0, index).toString(16).padStart(12, '0');
    const raw = `k${numeric}`;
    if (raw.length >= keySize) {
        return raw.slice(0, keySize);
    }
    return `${raw}${'_'.repeat(keySize - raw.length)}`;
}

export function keyBytes(index: number, keySize: number): Uint8Array {
    return new TextEncoder().encode(keyString(index, keySize));
}

export function valueBytes(index: number, valueSize: number): Uint8Array {
    const value = new Uint8Array(valueSize);
    let state = (index + 1) >>> 0;
    for (let i = 0; i < value.length; i += 1) {
        state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
        value[i] = state & 0xff;
    }
    return value;
}

export function scanWindow(workload: WorkloadSpec): { start: number; count: number } {
    const match = /^range_scan_(\d+)(?:_from_\d+[km]?)?$/.exec(workload.name);
    const count = match ? Number(match[1]) : Math.min(100, workload.recordCount);
    const start = Math.max(0, Math.floor((workload.recordCount - count) / 2));
    return { start, count };
}

export function randomReadCount(workload: WorkloadSpec): number {
    const explicit = /^random_get_(\d+)(k|m)?_/.exec(workload.name);
    if (explicit) {
        const value = Number(explicit[1]);
        const suffix = explicit[2];
        return suffix === 'm' ? value * 1_000_000 : suffix === 'k' ? value * 1_000 : value;
    }
    return Math.min(workload.recordCount, 10_000);
}

export function isBulkInsertWorkload(name: string): boolean {
    return (
        name.startsWith('bulk_insert_') ||
        name === 'cold_insert_1m_single_tx' ||
        name === 'batch_tx_100k_values_256b' ||
        name.startsWith('large_value_')
    );
}

export function isSdkPutSingleCallsWorkload(name: string): boolean {
    return /^sdk_put_\d+k?_single_calls$/.test(name);
}

export function isSingleTransactionInsertWorkload(name: string): boolean {
    return name === 'bulk_insert_1m_single_tx' || name === 'cold_insert_1m_single_tx';
}

export function isRandomGetWorkload(name: string): boolean {
    return name.startsWith('point_get_random_') || name.startsWith('random_get_');
}

export function isBulkRandomGetWorkload(name: string): boolean {
    return name.endsWith('_bulk');
}

export function isRangeScanWorkload(name: string): boolean {
    return name.startsWith('range_scan_');
}

export class DeterministicRng {
    #state: number;

    constructor(seed: number) {
        this.#state = seed >>> 0;
    }

    nextU32(): number {
        this.#state = (Math.imul(this.#state, 1664525) + 1013904223) >>> 0;
        return this.#state;
    }

    nextInt(exclusiveMax: number): number {
        if (exclusiveMax <= 0) {
            return 0;
        }
        return this.nextU32() % exclusiveMax;
    }
}
