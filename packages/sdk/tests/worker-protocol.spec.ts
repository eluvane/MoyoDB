import { expect, test, type Page } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';

declare global {
    interface Window {
        createWorkerProtocolHarness: () => Promise<{
            worker: Worker;
            client: import('../src/worker-client').WorkerProtocolClient;
            cleanup(): void;
        }>;
    }
}

const protocolHarnessScript = String.raw`
window.createWorkerProtocolHarness = async function createWorkerProtocolHarness() {
  const clientUrl = new URL('/src/worker-client.ts', window.location.href).href;
  const { WorkerProtocolClient } = await import(clientUrl);
  const workerServerUrl = new URL('/src/worker-server.ts', window.location.href).href;
  const workerBody = [
    "let snapshot = new Uint8Array([9, 8, 7]);",
    "let lastExport = null;",
    "let lastBulk = null;",
    "function stats() {",
    "  return {",
    "    db_name: 'test',",
    "    db_id: 1,",
    "    page_size: 4096,",
    "    catalog_root_page_id: 1,",
    "    next_page_id: 2,",
    "    last_committed_txid: 1,",
    "    last_replayed_wal_offset: 0,",
    "    store_count: 1,",
    "    manifest_len: lastExport ? lastExport.buffer.byteLength : 123,",
    "    main_len: lastBulk ? lastBulk.count : 0,",
    "    wal_len: lastBulk ? lastBulk.checksum : 0,",
    "    active_txns: 0,",
    "    write_tx_open: false,",
    "    cache_pages: 1",
    "  };",
    "}",
    "const api = {",
    "  async open() {},",
    "  async close() {},",
    "  async destroy() {},",
    "  async deleteDB() {},",
    "  async begin(mode) { return mode === 'readwrite' ? 7 : 3; },",
    "  async commit() { return 11; },",
    "  async rollback() {},",
    "  async createStore() {},",
    "  async dropStore() {},",
    "  async clearStore() {},",
    "  async get() { return snapshot.slice(); },",
    "  async getMany() { return [snapshot.slice(), null]; },",
    "  async has() { return true; },",
    "  async put() {},",
    "  async putMany(_txId, _store, entries) {",
    "    lastBulk = { count: entries.length, checksum: entries[0][0][0] + entries[entries.length - 1][1][0] };",
    "  },",
    "  async delete() { return true; },",
    "  async deleteMany(_txId, _store, keys) {",
    "    lastBulk = { count: keys.length, checksum: keys[0][0] + keys[keys.length - 1][0] };",
    "  },",
    "  async applyBatch(_txId, _store, ops) {",
    "    lastBulk = { count: ops.length, checksum: ops[0].key[0] + (ops[ops.length - 1].kind === 'put' ? ops[ops.length - 1].value[0] : ops[ops.length - 1].key[0]) };",
    "  },",
    "  async scan() { return [{ key: new Uint8Array([1]), value: snapshot.slice() }]; },",
    "  async getByIndex() { return snapshot.slice(); },",
    "  async scanByIndex() { return [{ key: new Uint8Array([2]), value: snapshot.slice() }]; },",
    "  async getIndexes() { return []; },",
    "  async reconcileIndexes() {},",
    "  async listStores() { return ['kv']; },",
    "  async getVersion() { return 1; },",
    "  async changesSince() { return { changes: [], latestTxId: 1 }; },",
    "  async setSchemaVersion() {},",
    "  async exportSnapshot() {",
    "    if (snapshot.byteLength === 3 && snapshot[0] === 2) {",
    "      return snapshot.slice();",
    "    }",
    "    lastExport = new Uint8Array(64 * 1024);",
    "    lastExport[0] = 42;",
    "    return lastExport;",
    "  },",
    "  async importSnapshot(data) { snapshot = data.slice(); },",
    "  async reset() {},",
    "  async compact() { return { sizeBefore: 1, sizeAfter: 1, reclaimedBytes: 0, durationMs: 0 }; },",
    "  async rebuild() { return { sizeBefore: 1, sizeAfter: 1, reclaimedBytes: 0, durationMs: 0 }; },",
    "  async stats() { return stats(); },",
    "  async storageInfo() { return await new Promise(() => {}); },",
    "  async requestPersistence() { return false; },",
    "  async setFailpoint() { throw { name: 'InjectedFailureError', code: 'InjectedFailureError', message: 'boom' }; }",
    "};",
    "exposeWorkerApi(api);"
  ].join('\n');
  const source = "import { exposeWorkerApi } from " + JSON.stringify(workerServerUrl) + ";\n" + workerBody;
  const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
  const worker = new Worker(url, { type: 'module' });
  URL.revokeObjectURL(url);
  const client = new WorkerProtocolClient(worker);
  await client.whenReady();
  return {
    worker,
    client,
    cleanup() {
      client.dispose();
      worker.terminate();
    }
  };
};
`;

async function prepareProtocolPage(page: Page): Promise<void> {
    await page.goto('/');
    await page.addScriptTag({ content: protocolHarnessScript });
}

test('worker_protocol_request_response', async ({ page }) => {
    await prepareProtocolPage(page);
    const value = await page.evaluate(async () => {
        const { client, cleanup } = await window.createWorkerProtocolHarness();
        try {
            return await client.begin('readwrite');
        } finally {
            cleanup();
        }
    });
    expect(value).toBe(7);
});

test('worker_protocol_error_response', async ({ page }) => {
    await prepareProtocolPage(page);
    const errorName = await page.evaluate(async () => {
        const { client, cleanup } = await window.createWorkerProtocolHarness();
        try {
            await client.setFailpoint('after_wal_flush');
            return 'NO_ERROR';
        } catch (error) {
            return (error as Error).name;
        } finally {
            cleanup();
        }
    });
    expect(errorName).toBe('InjectedFailureError');
});

test('worker_protocol_unknown_command', async ({ page }) => {
    await prepareProtocolPage(page);
    const result = await page.evaluate(async () => {
        const { worker, client, cleanup } = await window.createWorkerProtocolHarness();
        try {
            await client.whenReady();
            return await new Promise<{ ok: boolean; name: string; message: string }>((resolve) => {
                const id = 99;
                const onMessage = (event: MessageEvent) => {
                    const data = event.data;
                    if (data?.type !== 'moyodb:worker-protocol:response' || data.id !== id) {
                        return;
                    }
                    worker.removeEventListener('message', onMessage);
                    resolve({ ok: data.ok, name: data.error?.name, message: data.error?.message });
                };
                worker.addEventListener('message', onMessage);
                worker.postMessage({
                    type: 'moyodb:worker-protocol:request',
                    version: 1,
                    id,
                    command: 'definitelyMissing',
                    args: []
                });
            });
        } finally {
            cleanup();
        }
    });
    expect(result.ok).toBe(false);
    expect(result.name).toBe('WorkerProtocolError');
    expect(result.message).toContain('unsupported worker command');
});

test('worker_client_rejects_after_close', async ({ page }) => {
    await prepareProtocolPage(page);
    const errorName = await page.evaluate(async () => {
        const { client, cleanup } = await window.createWorkerProtocolHarness();
        try {
            await client.whenReady();
            client.dispose();
            await client.stats();
            return 'NO_ERROR';
        } catch (error) {
            return (error as Error).name;
        } finally {
            cleanup();
        }
    });
    expect(errorName).toBe('WorkerTerminatedError');
});

test('worker_pending_requests_reject_on_terminate', async ({ page }) => {
    await prepareProtocolPage(page);
    const errorMessage = await page.evaluate(async () => {
        const { client, worker, cleanup } = await window.createWorkerProtocolHarness();
        const pending = client.storageInfo();
        client.dispose(new Error('test termination'));
        worker.terminate();
        try {
            await pending;
            return 'NO_ERROR';
        } catch (error) {
            return (error as Error).message;
        } finally {
            cleanup();
        }
    });
    expect(errorMessage).toBe('test termination');
});

test('binary_payload_roundtrip', async ({ page }) => {
    await prepareProtocolPage(page);
    const result = await page.evaluate(async () => {
        const { client, cleanup } = await window.createWorkerProtocolHarness();
        const input = new Uint8Array([1, 2, 3, 4, 5]);
        try {
            await client.importSnapshot(input.subarray(1, 4));
            const output = await client.exportSnapshot();
            return {
                inputByteLength: input.byteLength,
                output: Array.from(output)
            };
        } finally {
            cleanup();
        }
    });
    expect(result.inputByteLength).toBe(5);
    expect(result.output).toEqual([2, 3, 4]);
});

test('packed_binary_response_roundtrip', async ({ page }) => {
    await prepareProtocolPage(page);
    const result = await page.evaluate(async () => {
        const { client, cleanup } = await window.createWorkerProtocolHarness();
        try {
            const many = await client.getMany(1, 'kv', [new Uint8Array([1]), new Uint8Array([2])]);
            const scan = await client.scan(1, 'kv', {});
            const indexScan = await client.scanByIndex(1, 'kv', 'by_test', {});
            return {
                many: many.map((value) => (value ? Array.from(value) : null)),
                scan: scan.map((row) => ({ key: Array.from(row.key), value: Array.from(row.value) })),
                indexScan: indexScan.map((row) => ({ key: Array.from(row.key), value: Array.from(row.value) }))
            };
        } finally {
            cleanup();
        }
    });
    expect(result.many).toEqual([[9, 8, 7], null]);
    expect(result.scan).toEqual([{ key: [1], value: [9, 8, 7] }]);
    expect(result.indexScan).toEqual([{ key: [2], value: [9, 8, 7] }]);
});

test('transferable_payload_roundtrip_if_supported', async ({ page }) => {
    await prepareProtocolPage(page);
    const result = await page.evaluate(async () => {
        const { client, cleanup } = await window.createWorkerProtocolHarness();
        try {
            const bytes = await client.exportSnapshot();
            const stats = await client.stats();
            return {
                receivedLength: bytes.byteLength,
                workerBufferLengthAfterPost: stats.manifest_len
            };
        } finally {
            cleanup();
        }
    });
    expect(result.receivedLength).toBe(64 * 1024);
    expect(result.workerBufferLengthAfterPost).toBe(0);
});

test('packed_bulk_binary_arguments_preserve_caller_buffers', async ({ page }) => {
    await prepareProtocolPage(page);
    const result = await page.evaluate(async () => {
        const { client, cleanup } = await window.createWorkerProtocolHarness();
        const keyA = new Uint8Array([9, 1, 2, 8]);
        const valueA = new Uint8Array([3, 4, 5]);
        const keyB = new Uint8Array([6, 7]);
        const valueB = new Uint8Array([11, 12, 13, 14]);
        try {
            await client.putMany(1, 'kv', [
                [keyA.subarray(1, 3), valueA],
                [keyB, valueB.subarray(1, 4)]
            ]);
            const afterPutMany = await client.stats();
            await client.deleteMany(1, 'kv', [keyA.subarray(1, 3), keyB]);
            const afterDeleteMany = await client.stats();
            await client.applyBatch(1, 'kv', [
                { kind: 'delete', key: keyA.subarray(1, 3) },
                { kind: 'put', key: keyB, value: valueB.subarray(1, 4) }
            ]);
            const afterBatch = await client.stats();
            return {
                callerBuffers: [keyA.byteLength, valueA.byteLength, keyB.byteLength, valueB.byteLength],
                putMany: { count: afterPutMany.main_len, checksum: afterPutMany.wal_len },
                deleteMany: { count: afterDeleteMany.main_len, checksum: afterDeleteMany.wal_len },
                batch: { count: afterBatch.main_len, checksum: afterBatch.wal_len }
            };
        } finally {
            cleanup();
        }
    });
    expect(result.callerBuffers).toEqual([4, 3, 2, 4]);
    expect(result.putMany).toEqual({ count: 2, checksum: 13 });
    expect(result.deleteMany).toEqual({ count: 2, checksum: 7 });
    expect(result.batch).toEqual({ count: 2, checksum: 13 });
});

test('put_get_through_worker_without_comlink', async ({ page }) => {
    const dbName = uniqueDbName('no-comlink-put-get');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('1'));
            const value = await db.get('kv', window.moyodb.utf8Encode('a'));
            return value ? window.moyodb.utf8Decode(value) : null;
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toBe('1');
});

test('transaction_through_worker_without_comlink', async ({ page }) => {
    const dbName = uniqueDbName('no-comlink-tx');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            const tx = await db.begin('readwrite');
            await tx.put('kv', window.moyodb.utf8Encode('a'), window.moyodb.utf8Encode('tx-value'));
            await tx.commit();
            const value = await db.get('kv', window.moyodb.utf8Encode('a'));
            return value ? window.moyodb.utf8Decode(value) : null;
        } finally {
            await db.close();
        }
    }, dbName);
    expect(result).toBe('tx-value');
});

test('recovery_open_through_worker_without_comlink', async ({ page }) => {
    const dbName = uniqueDbName('no-comlink-recovery');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            await db.put('kv', window.moyodb.utf8Encode('base'), window.moyodb.utf8Encode('ok'));
            await db.setFailpoint('after_wal_flush');
            try {
                await db.put('kv', window.moyodb.utf8Encode('after'), window.moyodb.utf8Encode('crashy'));
            } catch {
                // Expected failpoint.
            }
        } finally {
            await db.close().catch(() => undefined);
        }
        const reopened = await window.moyodb.openDB(name, { requestPersistence: false, ownerWaitMs: 2000 });
        try {
            const base = await reopened.get('kv', window.moyodb.utf8Encode('base'));
            return base ? window.moyodb.utf8Decode(base) : null;
        } finally {
            await reopened.close();
        }
    }, dbName);
    expect(result).toBe('ok');
});

test('worker_crash_cleanup', async ({ page }) => {
    const dbName = uniqueDbName('no-comlink-crash-cleanup');
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const db = await window.moyodb.openDB(name, { requestPersistence: false });
        try {
            await db.createStore('kv');
            const crashed = window.moyodb.unsafeDebugCrashWorker(name);
            let closedError = 'NO_ERROR';
            try {
                await db.stats();
            } catch (error) {
                closedError = (error as Error).name;
            }
            const reopened = await window.moyodb.openDB(name, { requestPersistence: false, ownerWaitMs: 2000 });
            await reopened.close();
            return { crashed, closedError };
        } finally {
            await db.close().catch(() => undefined);
        }
    }, dbName);
    expect(result).toEqual({ crashed: true, closedError: 'DatabaseClosedError' });
});
