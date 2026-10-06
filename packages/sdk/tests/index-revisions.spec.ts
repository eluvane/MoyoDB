import { expect, test } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';

test('native revision proofs survive managed writes, snapshots, rollback, import and rebuild', async ({ page }) => {
    await prepareMoyoDbPage(page);
    const result = await page.evaluate(async (name) => {
        const source = `
import { DbWorker } from ${JSON.stringify(new URL('/src/worker.ts', location.origin).href)};
import * as codec from ${JSON.stringify(new URL('/src/codec.ts', location.origin).href)};
import * as indexing from ${JSON.stringify(new URL('/src/indexing.ts', location.origin).href)};
import { encodeStoreValueRecord } from ${JSON.stringify(new URL('/src/compression.ts', location.origin).href)};
self.onmessage = async ({ data: name }) => {
    const options = { createIfMissing: true, ownerWaitMs: 0, requestPersistence: false, cachePages: 128, debugFailpoint: null, changeFeed: null };
    const persistence = { close() {}, persisted: async () => false, persist: async () => false };
    const worker = new DbWorker({ persistence });
    const target = new DbWorker({ persistence });
    const indexes = [{ store: 'docs', name: 'email', keyPath: 'email', unique: true }];
    const [def] = indexing.normalizeIndexDefinitions(indexes);
    const key = codec.u64Key(1);
    const candidate = codec.u64Key(2);
    let response;
    const value = (email, padding) => codec.jsonEncode({ email, padding: padding.repeat(4096) });
    const measure = async (db, txId, logical = 'same') => {
        const engine = db.engine;
        const originalGet = engine.get;
        const originalParse = JSON.parse;
        const originalRead = db.readStoreValue;
        if (typeof originalRead !== 'function') throw new Error('Missing store value reader');
        const counts = { bodyReads: 0, jsonParses: 0, valueReads: 0, error: '' };
        engine.get = function (...args) { if (args[1] === 'docs') counts.bodyReads++; return originalGet.apply(this, args); };
        JSON.parse = function (...args) { counts.jsonParses++; return originalParse.apply(this, args); };
        db.readStoreValue = async function (...args) {
            const value = await originalRead.apply(this, args);
            if (value !== null) counts.valueReads++;
            return value;
        };
        try {
            await db.assertUniqueIndexAvailability(txId, def, codec.indexKey(logical), candidate);
        } catch (error) {
            counts.error = error.name;
        } finally {
            engine.get = originalGet;
            JSON.parse = originalParse;
            db.readStoreValue = originalRead;
        }
        return counts;
    };
    try {
        await worker.open({ dbName: name, options });
        const seed = await worker.begin('readwrite');
        await worker.createStore(seed, 'docs', { compression: 'snappy' });
        await worker.reconcileIndexes(seed, indexes);
        await worker.put(seed, 'docs', key, value('same', 'a'));
        await worker.commit(seed);
        const oldReader = await worker.begin('readonly');
        const firstWriter = await worker.begin('readwrite');
        const known = await measure(worker, firstWriter);
        await worker.put(firstWriter, 'docs', key, value('same', 'b'));
        const refreshed = await measure(worker, firstWriter);
        worker.engine.put(BigInt(firstWriter), 'docs', key, await encodeStoreValueRecord(value('same', 'c'), 'snappy'));
        const rawFallback = await measure(worker, firstWriter);
        const primary = worker.readNativeValueState(firstWriter, 'docs', key);
        const forgedBody = new Uint8Array(24);
        forgedBody.set(codec.utf8Encode('IDXREV01'));
        const forgedView = new DataView(forgedBody.buffer);
        forgedView.setBigUint64(8, primary.revisionEpoch, true);
        forgedView.setBigUint64(16, primary.revisionOrdinal, true);
        worker.engine.put(BigInt(firstWriter), def.internalStore, indexing.encodeIndexEntryKey(codec.indexKey('forged'), key), forgedBody);
        const forged = await measure(worker, firstWriter, 'forged');
        await worker.rollback(firstWriter);
        const secondWriter = await worker.begin('readwrite');
        const rolledBack = await measure(worker, secondWriter);
        worker.engine.put(BigInt(secondWriter), 'docs', key, await encodeStoreValueRecord(value('same', 'd'), 'snappy'));
        await worker.reconcileIndexes(secondWriter, indexes);
        const reconciled = await measure(worker, secondWriter);
        await worker.commit(secondWriter);
        const oldSnapshot = await measure(worker, oldReader);
        await worker.rollback(oldReader);
        const expiryWriter = await worker.begin('readwrite');
        await worker.put(expiryWriter, 'docs', codec.u64Key(3), value('expired', 'e'), { ttl: 0 });
        const expired = await measure(worker, expiryWriter, 'expired');
        await worker.commit(expiryWriter);
        const snapshot = await worker.exportSnapshot();
        await target.open({ dbName: name + '-imported', options });
        await target.importSnapshot(snapshot);
        const importedWriter = await target.begin('readwrite');
        const imported = await measure(target, importedWriter);
        await target.rollback(importedWriter);
        await target.rebuild();
        const rebuiltWriter = await target.begin('readwrite');
        const rebuilt = await measure(target, rebuiltWriter);
        await target.rollback(rebuiltWriter);
        response = { known, refreshed, rawFallback, forged, rolledBack, reconciled, oldSnapshot, expired, imported, rebuilt };
    } catch (error) {
        response = { failed: String(error.stack ?? error) };
    } finally {
        await target.destroy().catch(() => {});
        await worker.destroy().catch(() => {});
    }
    self.postMessage(response);
};`;
        const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
        const worker = new Worker(url, { type: 'module' });
        try {
            return await new Promise<{
                failed?: string;
                [key: string]: unknown;
            }>((resolve, reject) => {
                worker.onmessage = (event: MessageEvent<{ failed?: string; [key: string]: unknown }>) =>
                    resolve(event.data);
                worker.onerror = (event) => reject(new Error(event.message));
                worker.postMessage(name);
            });
        } finally {
            worker.terminate();
            URL.revokeObjectURL(url);
        }
    }, uniqueDbName('index-native-revisions'));
    expect(result.failed).toBeUndefined();
    const trustedConflict = {
        bodyReads: 0,
        jsonParses: 0,
        valueReads: 0,
        error: 'UniqueIndexConstraintError'
    };
    for (const key of ['known', 'refreshed', 'rolledBack', 'reconciled', 'oldSnapshot', 'imported', 'rebuilt']) {
        expect(result[key], key).toEqual(trustedConflict);
    }
    expect(result.rawFallback).toEqual({ ...trustedConflict, bodyReads: 1, jsonParses: 1, valueReads: 1 });
    expect(result.forged).toEqual({ bodyReads: 1, jsonParses: 1, valueReads: 1, error: '' });
    expect(result.expired).toEqual({ bodyReads: 0, jsonParses: 0, valueReads: 0, error: '' });
});
