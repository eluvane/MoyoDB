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
        'Opt-in only. 10,000 separate commits on one thread; cost grows much faster than the record count. Pass this name via MOYODB_BENCH_WORKLOADS.',
        false,
        ['moyodb'],
        ['diagnostic', 'sdk', 'manual']
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
        'Opt-in only. 100 commits into a growing tree, repeated per sample. Pass this name via MOYODB_BENCH_WORKLOADS.',
        false,
        ['moyodb', 'indexeddb'],
        ['write', 'manual']
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
        ['write', 'full', 'manual']
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
        'Opt-in only. 1M insert with smaller commit batches. Pass this name via MOYODB_BENCH_WORKLOADS.',
        false,
        ['moyodb', 'indexeddb'],
        ['write', 'full', 'manual']
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
        'Opt-in only. 1M insert with larger commit batches. Pass this name via MOYODB_BENCH_WORKLOADS.',
        false,
        ['moyodb', 'indexeddb'],
        ['write', 'full', 'manual']
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
        ['write', 'full', 'pathological', 'manual']
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
        ['write', 'full', 'pathological', 'manual']
    ),

    workload(
        'point_get_random_10k',
        10_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; one readonly transaction; sequential: each of 10,000 gets is issued only after the previous one resolved',
        1,
        5,
        'Sequential point-read latency after a 10k-row setup. Both engines keep exactly one request outstanding.',
        true,
        ['moyodb', 'indexeddb'],
        ['read', 'sequential']
    ),
    workload(
        'point_get_random_10k_pipelined',
        10_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; one readonly transaction; pipelined: all 10,000 single-key gets are issued before any result is awaited',
        1,
        5,
        'Pipelined point-read throughput after a 10k-row setup. IndexedDB queues store.get requests; MoyoDB issues tx.get calls concurrently.',
        true,
        ['moyodb', 'indexeddb'],
        ['read', 'pipelined']
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
        'Bulk random point reads after a 10k-row setup. IndexedDB has no multi-key get, so compare this row with point_get_random_10k_pipelined for the IndexedDB best case.',
        true,
        ['moyodb'],
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
        'Opt-in only. Random point reads after a 100k-row setup. Pass this name via MOYODB_BENCH_WORKLOADS.',
        false,
        ['moyodb', 'indexeddb'],
        ['read', 'manual']
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
        ['read', 'full', 'manual']
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
        ['read', 'full', 'manual']
    ),
    workload(
        'random_get_10k_from_1m',
        1_000_000,
        16,
        256,
        5_000,
        'setup/preload outside measurement; measured region performs 10,000 sequential single random gets',
        0,
        3,
        'Layered read workload: 1M-row preload outside timed region, 10k sequential gets measured.',
        false,
        ['moyodb', 'indexeddb'],
        ['read', 'sequential', 'full', 'manual']
    ),
    workload(
        'random_get_10k_from_1m_pipelined',
        1_000_000,
        16,
        256,
        5_000,
        'setup/preload outside measurement; measured region issues 10,000 single random gets before awaiting any of them',
        0,
        3,
        'Layered read workload: same keys as the sequential row, pipelined in both engines.',
        false,
        ['moyodb', 'indexeddb'],
        ['read', 'pipelined', 'full', 'manual']
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
        'Layered read workload: same keys as the sequential row, one bulk read call. IndexedDB has no multi-key get; compare with the pipelined row.',
        false,
        ['moyodb'],
        ['read', 'bulk', 'full', 'manual']
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
        'Range scan over 100 rows. Both engines use their bulk range API: MoyoDB tx.scan, IndexedDB getAllKeys + getAll on the same range.',
        true,
        ['moyodb', 'indexeddb'],
        ['scan']
    ),
    workload(
        'reverse_scan_limit_1',
        10_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; measurement reads the last key of the store with a reverse scan limited to one row',
        1,
        5,
        'Reverse bounded scan: MoyoDB tx.scan({ reverse: true, limit: 1 }), IndexedDB openCursor(null, "prev") stopped after one row.',
        true,
        ['moyodb', 'indexeddb'],
        ['scan', 'reverse']
    ),
    workload(
        'range_scan_1000',
        10_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; measurement scans 1,000 contiguous keys from a 10k-row DB',
        1,
        5,
        'Range scan over 1,000 rows.',
        false,
        ['moyodb', 'indexeddb'],
        ['scan']
    ),
    workload(
        'range_scan_10000',
        10_000,
        16,
        256,
        1_000,
        'setup/preload outside measurement; measurement scans 10,000 contiguous keys from a 10k-row DB',
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
        ['scan', 'full', 'manual']
    ),

    workload(
        'small_tx_1000_commits',
        1_000,
        16,
        128,
        1,
        '1,000 independent readwrite commits; keys/values and empty DB setup outside measurement',
        0,
        1,
        'Small transaction commit overhead. One sample: each commit is its own OPFS flush.',
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
        'Opt-in only. 100k values in 20 commits. Pass this name via MOYODB_BENCH_WORKLOADS.',
        false,
        ['moyodb', 'indexeddb'],
        ['write', 'manual']
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
        'Opt-in only. Cold open after a 100k-row setup. Pass this name via MOYODB_BENCH_WORKLOADS.',
        false,
        ['moyodb', 'indexeddb'],
        ['open', 'manual']
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
        if (workload.tags?.includes('manual')) {
            return false;
        }
        if (profile === 'smoke') {
            return workload.smoke === true;
        }
        if (profile === 'standard') {
            return !workload.tags?.includes('full') && workload.name !== 'large_value_1mb';
        }
        return true;
    });
}

function keyString(index: number, keySize: number): string {
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

export function keyBytes(index: number, keySize: number): Uint8Array<ArrayBuffer> {
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

function rangeScanCount(name: string): number | null {
    const prefix = 'range_scan_';
    if (!name.startsWith(prefix)) {
        return null;
    }
    let index = prefix.length;
    const start = index;
    while (index < name.length && name[index] >= '0' && name[index] <= '9') {
        index += 1;
    }
    if (index === start) {
        return null;
    }
    const count = Number(name.slice(start, index));
    if (index === name.length) {
        return count;
    }
    const from = '_from_';
    if (!name.startsWith(from, index)) {
        return null;
    }
    index += from.length;
    const fromStart = index;
    while (index < name.length && name[index] >= '0' && name[index] <= '9') {
        index += 1;
    }
    if (index === fromStart) {
        return null;
    }
    if (index < name.length && (name[index] === 'k' || name[index] === 'm')) {
        index += 1;
    }
    return index === name.length ? count : null;
}
export function scanWindow(workload: WorkloadSpec): { start: number; count: number } {
    const count = rangeScanCount(workload.name) ?? Math.min(100, workload.recordCount);
    const start = Math.max(0, Math.floor((workload.recordCount - count) / 2));
    return { start, count };
}

function randomReadCount(workload: WorkloadSpec): number {
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

export type ReadRequestMode = 'sequential' | 'pipelined' | 'bulk';

export function readRequestMode(name: string): ReadRequestMode {
    if (name.endsWith('_bulk')) {
        return 'bulk';
    }
    return name.endsWith('_pipelined') ? 'pipelined' : 'sequential';
}

export function isRangeScanWorkload(name: string): boolean {
    return name.startsWith('range_scan_');
}

export function isReverseScanWorkload(name: string): boolean {
    return name === 'reverse_scan_limit_1';
}

export function isPreloadedReadWorkload(name: string): boolean {
    return isRandomGetWorkload(name) || isRangeScanWorkload(name) || isReverseScanWorkload(name);
}

/** Record indices every engine reads for one sample, in request order. */
export function randomReadIndices(workload: WorkloadSpec, sampleIndex: number): number[] {
    const rng = new DeterministicRng(0x5eed0000 ^ sampleIndex ^ workload.recordCount);
    const readCount = randomReadCount(workload);
    const indices: number[] = [];
    for (let i = 0; i < readCount; i += 1) {
        indices.push(rng.nextInt(workload.recordCount));
    }
    return indices;
}

/** Record indices a scan workload must return, in result order. */
export function scanResultIndices(workload: WorkloadSpec): number[] {
    if (isReverseScanWorkload(workload.name)) {
        return [workload.recordCount - 1];
    }
    const { start, count } = scanWindow(workload);
    return Array.from({ length: count }, (_, offset) => start + offset);
}

const WRITE_VERIFICATION_SAMPLE = 1024;

/** Evenly spaced record indices, including the first and last, read back after a write sample. */
export function writeVerificationIndices(workload: WorkloadSpec): number[] {
    const count = workload.recordCount;
    if (count <= WRITE_VERIFICATION_SAMPLE) {
        return Array.from({ length: count }, (_, index) => index);
    }
    const step = (count - 1) / (WRITE_VERIFICATION_SAMPLE - 1);
    return Array.from({ length: WRITE_VERIFICATION_SAMPLE }, (_, index) => Math.round(index * step));
}

export type VerificationKind = 'point-read' | 'scan' | 'write' | 'none';

export function verificationKind(name: string): VerificationKind {
    if (isRandomGetWorkload(name)) {
        return 'point-read';
    }
    if (isRangeScanWorkload(name) || isReverseScanWorkload(name)) {
        return 'scan';
    }
    if (
        isBulkInsertWorkload(name) ||
        isSdkPutSingleCallsWorkload(name) ||
        name === 'small_tx_1000_commits' ||
        name === 'sdk_bulk_put_10k' ||
        name === 'indexeddb_bulk_put_10k'
    ) {
        return 'write';
    }
    return 'none';
}

/** Order-sensitive FNV-1a over length-prefixed byte strings; `null` is its own marker. */
export class ContentChecksum {
    #hash = 0x811c9dc5;
    #items = 0;

    add(bytes: Uint8Array | null): void {
        const length = bytes ? bytes.length : 0xffffffff;
        this.#mix(length & 0xff);
        this.#mix((length >>> 8) & 0xff);
        this.#mix((length >>> 16) & 0xff);
        this.#mix((length >>> 24) & 0xff);
        if (bytes) {
            for (const byte of bytes) {
                this.#mix(byte);
            }
        }
        this.#items += 1;
    }

    digest(): string {
        return `${this.#items}:${this.#hash.toString(16).padStart(8, '0')}`;
    }

    #mix(byte: number): void {
        this.#hash = Math.imul(this.#hash ^ byte, 0x01000193) >>> 0;
    }
}

export function expectedValuesChecksum(workload: WorkloadSpec, indices: number[]): string {
    const checksum = new ContentChecksum();
    for (const index of indices) {
        checksum.add(valueBytes(index, workload.valueSize));
    }
    return checksum.digest();
}

export function expectedRowsChecksum(workload: WorkloadSpec, indices: number[]): string {
    const checksum = new ContentChecksum();
    for (const index of indices) {
        checksum.add(keyBytes(index, workload.keySize));
        checksum.add(valueBytes(index, workload.valueSize));
    }
    return checksum.digest();
}

export function assertChecksum(label: string, actual: ContentChecksum, expected: string): string {
    const digest = actual.digest();
    if (digest !== expected) {
        throw new Error(`${label} content checksum mismatch: got ${digest}, expected ${expected}`);
    }
    return digest;
}

class DeterministicRng {
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
