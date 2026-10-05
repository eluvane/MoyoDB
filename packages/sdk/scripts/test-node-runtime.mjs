import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readdir, rm } from 'node:fs/promises';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
const originalSelf = Object.getOwnPropertyDescriptor(globalThis, 'self');
const sdk = await import('@moyodb/sdk/node');
const script = fileURLToPath(import.meta.url);
const {
    openNodeDB,
    deleteNodeDB,
    unsafeDebugCrashNodeWorker,
    utf8Encode,
    utf8Decode,
    jsonEncode,
    jsonDecode,
    indexKey
} = sdk;
const key = utf8Encode('key');
const text = (value) => (value === null ? null : utf8Decode(value));
const childCommand = process.argv[2];

if (childCommand) {
    const directory = process.argv[3];
    const name = process.argv[4];
    if (childCommand === 'busy') {
        await assert.rejects(openNodeDB(name, { directory }), { name: 'DatabaseBusyError' });
    } else if (childCommand === 'timeout') {
        const started = performance.now();
        await assert.rejects(openNodeDB(name, { directory, ownerWaitMs: 100 }), { name: 'DatabaseBusyError' });
        assert.ok(performance.now() - started >= 100);
    } else {
        if (childCommand === 'wait') process.send({ started: true });
        const db = await openNodeDB(name, { directory, ownerWaitMs: childCommand === 'wait' ? 3000 : 0 });
        try {
            if (childCommand === 'seed') {
                await db.createStore('data');
                await db.put('data', key, utf8Encode('persisted'));
            } else if (childCommand === 'read') {
                assert.equal(text(await db.get('data', key)), 'crash-safe');
            } else if (childCommand === 'hold') {
                await db.createStore('data');
                await db.put('data', key, utf8Encode('unclean-process'));
                process.send({ ready: true });
                await once(process, 'message');
            } else if (childCommand === 'wait') {
                assert.equal(text(await db.get('data', key)), 'persisted');
            } else if (childCommand === 'interop-create' || childCommand === 'interop-create-compacted') {
                await db.createStore('kv');
                await db.put('kv', utf8Encode('interop'), utf8Encode('shared-file-format'));
                if (childCommand === 'interop-create-compacted') {
                    await db.compact();
                    await db.rebuild();
                }
            } else if (childCommand === 'interop-read' || childCommand === 'interop-read-native-append') {
                assert.equal(text(await db.get('kv', utf8Encode('interop'))), 'shared-file-format');
                if (childCommand === 'interop-read-native-append') {
                    assert.equal(text(await db.get('kv', utf8Encode('native-append'))), 'shared-file-format');
                }
            } else {
                throw new Error(`unknown child command: ${childCommand}`);
            }
        } finally {
            await db.close();
        }
    }
} else {
    const temporaryRoot = resolve(dirname(script), '../../../.tmp');
    await mkdir(temporaryRoot, { recursive: true });
    const output = await mkdtemp(join(temporaryRoot, 'node-runtime-'));
    const directory = join(output, 'primary');
    const name = 'node-persistence';
    const handles = new Set();
    const children = new Set();
    const deadline = setTimeout(() => {
        process.stderr.write('Node runtime test timed out\n');
        for (const child of children) child.kill();
        process.exit(1);
    }, 45_000);

    function createChild(command, storageDirectory = directory, dbName = name) {
        const child = spawn(process.execPath, [script, command, storageDirectory, dbName], {
            stdio: ['ignore', 'pipe', 'pipe', 'ipc'],
            windowsHide: true
        });
        children.add(child);
        let errors = '';
        child.stderr.setEncoding('utf8').on('data', (data) => {
            errors += data;
        });
        child.stdout.resume();
        const exited = new Promise((resolve, reject) => {
            child.once('error', reject);
            child.once('exit', (code, signal) => {
                children.delete(child);
                resolve({ code, signal, errors });
            });
        });
        return { child, exited };
    }

    async function runChild(command) {
        const { exited } = createChild(command);
        const result = await exited;
        assert.equal(result.code, 0, result.errors);
    }

    async function track(name, options) {
        const db = await openNodeDB(name, options);
        handles.add(db);
        return db;
    }

    try {
        await assert.rejects(openNodeDB('', { directory }), TypeError);
        await assert.rejects(openNodeDB('\ud800', { directory }), TypeError);
        await assert.rejects(openNodeDB('x'.repeat(128), { directory }), TypeError);
        await assert.rejects(openNodeDB(name, {}), { name: 'InvalidOpenOptionsError' });
        await assert.rejects(openNodeDB(name, { directory, version: -1 }), { name: 'InvalidOpenOptionsError' });
        await runChild('seed');

        const layout = await readdir(join(directory, 'stackdb', Buffer.from(name, 'utf8').toString('hex')));
        assert.ok(layout.includes('main.bin'));
        assert.equal(layout.includes('stackdb'), false);
        let db = await track(name, { directory, createIfMissing: false });
        assert.equal(text(await db.get('data', key)), 'persisted');
        await runChild('busy');
        await runChild('timeout');
        const sameDb = await track(name, { directory: join(directory, '.') });
        await assert.rejects(openNodeDB(name, { directory, cachePages: 32 }), { name: 'InvalidOpenOptionsError' });
        await sameDb.close();
        assert.equal(text(await db.get('data', key)), 'persisted');

        const tx = await db.begin('readwrite');
        await tx.putMany('data', [
            [utf8Encode('a'), utf8Encode('one')],
            [utf8Encode('b'), utf8Encode('two')]
        ]);
        await tx.applyBatch('data', [{ kind: 'delete', key: utf8Encode('b') }]);
        assert.deepEqual((await tx.getMany('data', [utf8Encode('a'), utf8Encode('b')])).map(text), ['one', null]);
        await tx.rollback();
        await assert.rejects(tx.get('data', key), { name: 'TransactionClosedError' });
        assert.equal(await db.get('data', utf8Encode('a')), null);

        const binary = new Uint8Array([0, 255, 1, 128]);
        const committed = await db.begin('readwrite');
        await committed.put('data', utf8Encode('binary'), binary);
        await committed.commit();
        assert.deepEqual(binary, new Uint8Array([0, 255, 1, 128]));
        assert.deepEqual(await db.get('data', utf8Encode('binary')), binary);
        const info = await db.storageInfo();
        assert.equal(info.persisted, true);
        assert.ok(info.dbSize > 0);
        assert.equal(await db.requestPersistence(), true);

        const waiter = createChild('wait');
        await once(waiter.child, 'message');
        await new Promise((resolve) => setTimeout(resolve, 100));
        await db.close();
        const waited = await waiter.exited;
        assert.equal(waited.code, 0, waited.errors);
        db = await track(name, { directory });

        const firstReader = await db.begin();
        const secondReader = await db.begin();
        const firstWriter = await db.begin('readwrite');
        const secondWriter = await db.begin('readwrite');
        const concurrentKey = utf8Encode('concurrent');
        await firstWriter.put('data', concurrentKey, utf8Encode('committed'));
        await secondWriter.put('data', concurrentKey, utf8Encode('conflicting'));
        await firstWriter.commit();
        await assert.rejects(secondWriter.commit(), { name: 'TransactionConflictError' });
        assert.equal(await firstReader.get('data', concurrentKey), null);
        assert.equal(await secondReader.get('data', concurrentKey), null);
        await firstReader.rollback();
        await secondReader.rollback();
        assert.equal(text(await db.get('data', concurrentKey)), 'committed');

        const other = await track(name, { directory: join(output, 'other') });
        await other.createStore('data');
        await other.put('data', key, utf8Encode('isolated'));
        const events = [];
        const unsubscribe = other.subscribe('data', (_store, changes) => events.push(changes));
        await db.put('data', key, utf8Encode('crash-safe'));
        await other.put('data', key, utf8Encode('independent'));
        await new Promise((resolve, reject) => {
            const timeout = setTimeout(() => reject(new Error('Node subscription did not receive commit')), 3000);
            const finish = () => {
                if (events.length === 0) return;
                clearTimeout(timeout);
                resolve();
            };
            const stop = other.subscribe('data', () => {
                stop();
                finish();
            });
            void other.put('data', utf8Encode('event'), utf8Encode('seen')).catch(reject);
        });
        unsubscribe();
        assert.equal(events.length, 2);
        assert.equal(text(await other.get('data', key)), 'independent');
        await other.close();

        const abandoned = await db.begin('readwrite');
        await abandoned.put('data', key, utf8Encode('uncommitted'));
        assert.equal(await unsafeDebugCrashNodeWorker(name, { directory }), true);
        await assert.rejects(db.get('data', key), { name: 'DatabaseClosedError' });
        await assert.rejects(abandoned.commit(), { name: 'TransactionClosedError' });
        await db.close();
        await runChild('read');
        const reopened = await track(name, { directory });
        assert.equal(text(await reopened.get('data', key)), 'crash-safe');
        await deleteNodeDB(name, { directory });
        await assert.rejects(reopened.get('data', key), { name: 'DatabaseClosedError' });
        await assert.rejects(openNodeDB(name, { directory, createIfMissing: false }));

        const indexed = await track('indexed', {
            directory,
            version: 1,
            indexes: [{ store: 'users', name: 'email', keyPath: 'email', unique: true }],
            migrate: async ({ transaction }) => {
                await transaction.createStore('users', { compression: 'gzip' });
            }
        });
        await indexed.put('users', key, jsonEncode({ email: 'node@example.test', label: 'Node' }));
        const reader = await indexed.begin();
        const found = await reader.getByIndex('users', 'email', indexKey('node@example.test'));
        assert.equal(jsonDecode(found).label, 'Node');
        await reader.rollback();
        const snapshot = await indexed.exportSnapshot({ compression: 'gzip' });
        await indexed.reset();
        await indexed.importSnapshot(snapshot);
        await indexed.compact();
        await indexed.rebuild();
        assert.equal(jsonDecode(await indexed.get('users', key)).label, 'Node');
        const records = await sdk.createRecordStore(indexed, 'records', {
            key: sdk.keyCodecs.string,
            value: sdk.jsonRecordCodec()
        });
        await records.put('typed', { label: 'typed record' });
        assert.deepEqual(await records.get('typed'), { label: 'typed record' });
        const sql = sdk.createSqlClient(indexed);
        await sql.execute('CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT NOT NULL)');
        await sql.execute('INSERT INTO notes (id, title) VALUES (?, ?)', [1n, 'Node SQL']);
        assert.deepEqual(await sql.query('SELECT title FROM notes WHERE id = ?', [1n]), [{ title: 'Node SQL' }]);
        assert.equal((await sql.explain('SELECT title FROM notes WHERE id = ?', [1n])).access, 'primary-key-lookup');
        await indexed.destroy();

        const unclean = createChild('hold', directory, 'process-crash');
        await once(unclean.child, 'message');
        unclean.child.kill();
        await unclean.exited;
        const recovered = await track('process-crash', { directory });
        assert.equal(text(await recovered.get('data', key)), 'unclean-process');
        await recovered.close();

        assert.deepEqual(Object.getOwnPropertyDescriptor(globalThis, 'navigator'), originalNavigator);
        assert.deepEqual(Object.getOwnPropertyDescriptor(globalThis, 'self'), originalSelf);
        assert.ok((await readdir(join(directory, 'stackdb'))).length >= 2);
        process.stdout.write(
            'Node runtime: persistence, transactions, isolation, indexes, snapshots, records, SQL and recovery passed\n'
        );
    } finally {
        clearTimeout(deadline);
        await Promise.allSettled(
            Array.from(children, async (child) => {
                const exited = once(child, 'exit');
                child.kill();
                await exited;
            })
        );
        await Promise.allSettled(Array.from(handles, (db) => db.close()));
        const target = relative(temporaryRoot, output);
        assert.ok(target.length > 0 && target !== '..' && !target.startsWith(`..${sep}`));
        await rm(output, { recursive: true, force: true });
    }
}
