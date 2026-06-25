import { test, expect } from '@playwright/test';
import { prepareMoyoDbPage, uniqueDbName } from './support';

type ProcessLike = { env?: Record<string, string | undefined> };
const proc = (globalThis as { process?: ProcessLike }).process;
const lifecycle = proc?.env?.npm_lifecycle_event ?? '';
const shouldRunSoak =
    proc?.env?.MOYODB_RUN_SOAK === '1' || lifecycle === 'test:soak' || lifecycle === 'test:browser:soak';
const soakMs = Number(proc?.env?.MOYODB_SOAK_MS ?? '30000');
const soakSeed = Number(proc?.env?.MOYODB_SOAK_SEED ?? '12648430');

test.describe('long-running soak/model browser tests', () => {
    test.skip(!shouldRunSoak, 'soak test is opt-in; use npm run test:soak or MOYODB_RUN_SOAK=1');

    test('random operations match JS Map across close/reopen and snapshot import', async ({ page }) => {
        test.setTimeout(Math.max(120_000, soakMs + 60_000));
        const dbName = uniqueDbName('soak');
        await prepareMoyoDbPage(page);
        const result = await page.evaluate(
            async ({ name, durationMs, seed }) => {
                class Rng {
                    state: number;
                    constructor(seed: number) {
                        this.state = seed >>> 0;
                    }
                    nextU32(): number {
                        this.state = (Math.imul(this.state, 1664525) + 1013904223) >>> 0;
                        return this.state;
                    }
                    nextInt(max: number): number {
                        return max <= 0 ? 0 : this.nextU32() % max;
                    }
                }
                const encode = window.moyodb.utf8Encode;
                const decode = window.moyodb.utf8Decode;
                const rng = new Rng(seed);
                const model = new Map<string, string>();
                let db = await window.moyodb.openDB(name, { requestPersistence: false });
                await db.createStore('kv');
                let ops = 0;
                let reopenCount = 0;
                let snapshotChecks = 0;
                const started = performance.now();

                async function verify(currentDb = db): Promise<void> {
                    const rows = await currentDb.scan('kv');
                    const actual = new Map(rows.map((row) => [decode(row.key), decode(row.value)]));
                    if (actual.size !== model.size) {
                        throw new Error(`model size mismatch: actual=${actual.size} expected=${model.size}`);
                    }
                    for (const [key, value] of model) {
                        if (actual.get(key) !== value) {
                            throw new Error(`model mismatch for ${key}: actual=${actual.get(key)} expected=${value}`);
                        }
                    }
                }

                while (performance.now() - started < durationMs) {
                    const op = rng.nextInt(100);
                    const key = `k-${rng.nextInt(200).toString().padStart(4, '0')}`;
                    if (op < 45) {
                        const value = `v-${ops}-${rng.nextU32().toString(16)}`;
                        await db.put('kv', encode(key), encode(value));
                        model.set(key, value);
                    } else if (op < 70) {
                        const deleted = await db.delete('kv', encode(key));
                        const expected = model.delete(key);
                        if (deleted !== expected) {
                            throw new Error(`delete mismatch for ${key}: actual=${deleted} expected=${expected}`);
                        }
                    } else if (op < 90) {
                        const value = await db.get('kv', encode(key));
                        const actual = value ? decode(value) : null;
                        const expected = model.get(key) ?? null;
                        if (actual !== expected) {
                            throw new Error(`get mismatch for ${key}: actual=${actual} expected=${expected}`);
                        }
                    } else {
                        await verify();
                    }

                    ops += 1;
                    if (ops % 50 === 0) {
                        await verify();
                        await db.close();
                        reopenCount += 1;
                        db = await window.moyodb.openDB(name, { requestPersistence: false });
                    }
                    if (ops % 125 === 0) {
                        const snapshot = await db.exportSnapshot();
                        const importName = `${name}-snapshot-${snapshotChecks}`;
                        const imported = await window.moyodb.openDB(importName, { requestPersistence: false });
                        try {
                            await imported.importSnapshot(snapshot);
                            await verify(imported);
                        } finally {
                            await imported.close();
                            await window.moyodb.deleteDB(importName).catch(() => undefined);
                        }
                        snapshotChecks += 1;
                    }
                }
                await verify();
                await db.close();
                return { ops, reopenCount, snapshotChecks, seed };
            },
            { name: dbName, durationMs: soakMs, seed: soakSeed }
        );
        expect(result.ops).toBeGreaterThan(0);
        expect(result.seed).toBe(soakSeed);
    });
});
