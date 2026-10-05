import assert from 'node:assert/strict';
import fs from 'node:fs';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { once } from 'node:events';
import { dirname, join, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { isMainThread, parentPort, Worker, workerData } from 'node:worker_threads';
import { installNodeStorage, releaseNodeStorageLease } from '../src/node-storage.mjs';

const script = fileURLToPath(import.meta.url);
const temporaryRoot = resolve(dirname(script), '../../../.tmp');
const failure = () => Object.assign(new Error('injected filesystem failure'), { code: 'EACCES' });
const lockDirectory = (directory) => join(directory, '.moyodb.lock');

async function scenario() {
    const { action, directory, lockToken } = workerData;
    const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
    const originals = new Map();
    const replace = (method, replacement) => {
        originals.set(method, fs[method]);
        fs[method] = replacement;
    };
    const restore = () => {
        for (const [method, original] of originals) fs[method] = original;
        originals.clear();
    };
    const pause = () => {
        parentPort.postMessage({ kind: 'ready' });
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0);
    };
    let installation;
    try {
        if (action.startsWith('pause-')) {
            if (action === 'pause-before-record') {
                const open = fs.openSync;
                replace('openSync', (path, ...args) => {
                    if (String(path).endsWith(`${lockToken}.json`)) pause();
                    return open(path, ...args);
                });
            } else {
                const rename = fs.renameSync;
                replace('renameSync', (from, to) => {
                    if (action === 'pause-before-publish') pause();
                    const result = rename(from, to);
                    if (action === 'pause-after-publish') pause();
                    return result;
                });
            }
        }
        if (action === 'install-failure') {
            Object.defineProperty(globalThis, 'navigator', { configurable: false, value: {} });
            await assert.rejects(installNodeStorage(directory, { lockToken }), TypeError);
            assert.equal(fs.existsSync(lockDirectory(directory)), false);
            assert.deepEqual(fs.readdirSync(directory), []);
            return;
        }
        if (action === 'metadata-failure') {
            const flush = fs.fsyncSync;
            replace('fsyncSync', () => {
                throw failure();
            });
            await assert.rejects(installNodeStorage(directory, { lockToken }), { code: 'EACCES' });
            fs.fsyncSync = flush;
            assert.deepEqual(fs.readdirSync(directory), []);
            installation = await installNodeStorage(directory, { lockToken });
            return;
        }
        if (action === 'locked') {
            await assert.rejects(installNodeStorage(directory, { lockToken }), { name: 'DatabaseBusyError' });
            assert.deepEqual(Object.getOwnPropertyDescriptor(globalThis, 'navigator'), originalNavigator);
            return;
        }
        if (action === 'invalid-path') {
            await assert.rejects(installNodeStorage(directory, { lockToken }), { name: 'StorageError' });
            assert.deepEqual(Object.getOwnPropertyDescriptor(globalThis, 'navigator'), originalNavigator);
            return;
        }
        const encodedDbName = action === 'mounted-generations' ? '666f6f' : undefined;
        installation = await installNodeStorage(directory, { lockToken, encodedDbName });
        if (action === 'interop-owner') {
            parentPort.postMessage({ kind: 'ready' });
            await once(parentPort, 'message');
            return;
        }
        if (action === 'pause-after-unlink') {
            const unlink = fs.unlinkSync;
            replace('unlinkSync', (path) => {
                const result = unlink(path);
                if (String(path).endsWith(`${lockToken}.json`)) pause();
                return result;
            });
            installation.close();
        }
        const opfs = await import('../../../crates/engine/js/opfs_shim.js');
        const root = await navigator.storage.getDirectory();
        if (action === 'write' || action === 'hold') {
            const { sessionId } = await opfs.opfsOpenActiveDb('666f6f');
            opfs.opfsWriteAt(sessionId, 1, 0n, new Uint8Array([3, 5, 7, 9]));
            const flush = fs.fsyncSync;
            let flushes = 0;
            replace('fsyncSync', (fd) => {
                flushes += 1;
                return flush(fd);
            });
            opfs.opfsFlush(sessionId, 1);
            assert.equal(flushes, 1);
            restore();
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 4n);
            if (action === 'hold') {
                parentPort.postMessage({ kind: 'ready' });
                await once(parentPort, 'message');
            } else {
                opfs.opfsCloseSession(sessionId);
            }
        } else if (action === 'reopen') {
            const { sessionId } = await opfs.opfsOpenActiveDb('666f6f', false);
            assert.deepEqual([...opfs.opfsReadAt(sessionId, 1, 0n, 4)], [3, 5, 7, 9]);
            assert.equal(await opfs.opfsDbDirectorySize('666f6f'), 4);
            opfs.opfsTruncate(sessionId, 1, 6n);
            assert.deepEqual([...opfs.opfsReadAt(sessionId, 1, 0n, 6)], [3, 5, 7, 9, 0, 0]);
            opfs.opfsFlush(sessionId, 1);
            opfs.opfsCloseSession(sessionId);
            await opfs.opfsRemoveDb('666f6f');
            assert.equal(await opfs.opfsDbDirectorySize('666f6f'), 0);
        } else if (action === 'mounted-generations') {
            const { sessionId } = await opfs.opfsOpenActiveDb('666f6f');
            opfs.opfsWriteAt(sessionId, 1, 0n, new Uint8Array([53]));
            opfs.opfsFlush(sessionId, 1);
            opfs.opfsCloseSession(sessionId);
            assert.deepEqual([...fs.readFileSync(join(directory, 'main.bin'))], [53]);
            assert.equal(fs.existsSync(join(directory, 'stackdb')), false);
            const target = await opfs.opfsPrepareRebuildTarget('666f6f');
            const generation = await opfs.opfsOpenGenerationDb('666f6f', target.generationName);
            opfs.opfsWriteAt(generation.sessionId, 1, 0n, new Uint8Array([59, 61]));
            opfs.opfsFlush(generation.sessionId, 1);
            opfs.opfsCloseSession(generation.sessionId);
            await opfs.opfsSwapActiveGeneration('666f6f', target.generationName, null);
            assert.equal(await opfs.opfsReadActiveGeneration('666f6f'), target.generationName);
            await opfs.opfsCleanupInactiveEntries('666f6f');
            assert.equal(fs.existsSync(join(directory, 'main.bin')), false);
            const active = await opfs.opfsOpenActiveDb('666f6f', false);
            assert.deepEqual([...opfs.opfsReadAt(active.sessionId, 1, 0n, 2)], [59, 61]);
            opfs.opfsCloseSession(active.sessionId);
            const stackdb = await root.getDirectoryHandle('stackdb');
            await assert.rejects(stackdb.getDirectoryHandle('another-db', { create: true }), { name: 'NotFoundError' });
            await opfs.opfsRemoveDb('666f6f');
            assert.equal(await opfs.opfsDbDirectorySize('666f6f'), 0);
            assert.deepEqual(fs.readdirSync(directory), ['.moyodb.lock']);
        } else if (action === 'handles') {
            const directoryHandle = await root.getDirectoryHandle('data', { create: true });
            const file = await directoryHandle.getFileHandle('main.bin', { create: true });
            const handle = await file.createSyncAccessHandle();
            await assert.rejects(file.createSyncAccessHandle(), { name: 'NoModificationAllowedError' });
            await assert.rejects(root.removeEntry('data', { recursive: true }), { name: 'NoModificationAllowedError' });
            assert.throws(() => handle.write(new Uint8Array(1), { at: -1 }), RangeError);
            assert.throws(() => handle.truncate(Number.MAX_SAFE_INTEGER + 1), RangeError);
            handle.write(new Uint8Array([11, 13]), { at: 3 });
            handle.flush();
            handle.close();
            handle.close();
            assert.throws(() => handle.getSize(), { name: 'InvalidStateError' });
            const reopened = await file.createSyncAccessHandle();
            const data = new Uint8Array(5);
            assert.equal(reopened.read(data), 5);
            assert.deepEqual([...data], [0, 0, 0, 11, 13]);
            reopened.close();
            await root.removeEntry('data', { recursive: true });
            await assert.rejects(root.getDirectoryHandle('data'), { name: 'NotFoundError' });
        } else if (action === 'partial-open') {
            const { sessionId } = await opfs.opfsOpenActiveDb('666f6f');
            opfs.opfsWriteAt(sessionId, 1, 0n, new Uint8Array([23, 29]));
            opfs.opfsFlush(sessionId, 1);
            opfs.opfsCloseSession(sessionId);
            const open = fs.openSync;
            const close = fs.closeSync;
            const opened = new Set();
            const closed = new Set();
            replace('openSync', (path, flags, ...args) => {
                if (
                    flags === (fs.constants.O_RDWR | (fs.constants.O_NOFOLLOW ?? 0)) &&
                    String(path).endsWith('main.bin')
                ) {
                    throw failure();
                }
                const fd = open(path, flags, ...args);
                if (typeof flags === 'number' && (flags & fs.constants.O_RDWR) !== 0) opened.add(fd);
                return fd;
            });
            replace('closeSync', (fd) => {
                if (opened.has(fd)) closed.add(fd);
                return close(fd);
            });
            await assert.rejects(opfs.opfsOpenActiveDb('666f6f'), { code: 'EACCES' });
            assert.equal(opened.size, 2);
            assert.deepEqual(closed, opened);
            restore();
            const reopened = await opfs.opfsOpenActiveDb('666f6f', false);
            assert.deepEqual([...opfs.opfsReadAt(reopened.sessionId, 1, 0n, 2)], [23, 29]);
            opfs.opfsCloseSession(reopened.sessionId);
        } else if (action === 'filesystem-errors') {
            const directoryHandle = await root.getDirectoryHandle('data', { create: true });
            await directoryHandle.getFileHandle('main.bin', { create: true });
            const file = await directoryHandle.getFileHandle('main.bin');
            const handle = await file.createSyncAccessHandle();
            for (const [method, operation] of [
                ['writeSync', () => handle.write(new Uint8Array([1]))],
                ['readSync', () => handle.read(new Uint8Array(1))],
                ['fsyncSync', () => handle.flush()],
                ['ftruncateSync', () => handle.truncate(0)],
                ['fstatSync', () => handle.getSize()]
            ]) {
                replace(method, () => {
                    throw failure();
                });
                assert.throws(operation, { code: 'EACCES' });
                restore();
            }
            handle.write(new Uint8Array([31]));
            handle.flush();
            handle.close();
            replace('unlinkSync', () => {
                throw failure();
            });
            await assert.rejects(directoryHandle.removeEntry('main.bin'), { code: 'EACCES' });
            restore();
            assert.deepEqual([...fs.readFileSync(join(directory, 'data/main.bin'))], [31]);
        } else if (action === 'names-and-links') {
            for (const name of [
                '',
                '.',
                '..',
                '../outside',
                'a/b',
                'a\\b',
                'a\0b',
                'CON',
                'NUL.bin',
                'file.',
                'file ',
                '.moyodb.lock'
            ]) {
                await assert.rejects(root.getDirectoryHandle(name, { create: true }), TypeError);
                await assert.rejects(root.getFileHandle(name, { create: true }), TypeError);
                await assert.rejects(root.removeEntry(name, { recursive: true }), TypeError);
            }
            await assert.rejects(root.getDirectoryHandle('linked', { create: true }), { name: 'SecurityError' });
            await assert.rejects(root.removeEntry('linked', { recursive: true }), { name: 'SecurityError' });
            await assert.rejects(root.getFileHandle('linked.bin', { create: true }), { name: 'SecurityError' });
            await root.getFileHandle('file.bin', { create: true });
            await assert.rejects(root.getDirectoryHandle('file.bin'), { name: 'TypeMismatchError' });
            await root.getDirectoryHandle('folder', { create: true });
            await assert.rejects(root.getFileHandle('folder', { create: true }), { name: 'TypeMismatchError' });
        }
    } finally {
        restore();
        if (installation) {
            installation.close();
            installation.close();
            assert.deepEqual(Object.getOwnPropertyDescriptor(globalThis, 'navigator'), originalNavigator);
            assert.equal(fs.existsSync(lockDirectory(directory)), false);
        }
    }
}

function launch(action, directory, lockToken = randomUUID()) {
    const worker = new Worker(new URL(import.meta.url), { workerData: { action, directory, lockToken } });
    return { worker, lockToken };
}

function completion(worker, kind = 'done') {
    return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => {
            cleanup();
            reject(new Error(`worker did not report ${kind}`));
        }, 10_000);
        const message = (result) => {
            if (result.kind === 'failure') {
                cleanup();
                reject(Object.assign(new Error(result.message), result));
            } else if (result.kind === kind) {
                cleanup();
                resolve(result);
            }
        };
        const error = (failure) => {
            cleanup();
            reject(failure);
        };
        const exit = (code) => {
            cleanup();
            reject(new Error(`worker exited before ${kind}: ${code}`));
        };
        const cleanup = () => {
            clearTimeout(timeout);
            worker.off('message', message);
            worker.off('error', error);
            worker.off('exit', exit);
        };
        worker.on('message', message);
        worker.on('error', error);
        worker.on('exit', exit);
    });
}

async function run(action, directory) {
    const { worker, lockToken } = launch(action, directory);
    try {
        await completion(worker);
    } finally {
        await worker.terminate();
        releaseNodeStorageLease(directory, lockToken);
    }
}

if (!isMainThread) {
    try {
        await scenario();
        parentPort.postMessage({ kind: 'done' });
    } catch (error) {
        parentPort.postMessage({ kind: 'failure', message: error.stack ?? String(error), name: error.name });
    } finally {
        parentPort.close();
    }
} else if (process.argv[2] === '--process-owner') {
    const { worker } = launch('hold', process.argv[3]);
    await completion(worker, 'ready');
    process.stdout.write('ready\n');
} else if (process.argv[2] === '--interop-probe') {
    const directory = process.argv[3];
    const { worker, lockToken } = launch('interop-probe', directory);
    try {
        await completion(worker);
    } catch (error) {
        if (error.name === 'DatabaseBusyError') process.exitCode = 2;
        else throw error;
    } finally {
        await worker.terminate();
        releaseNodeStorageLease(directory, lockToken);
    }
} else if (process.argv[2] === '--interop-owner') {
    const directory = process.argv[3];
    const eof = once(process.stdin, 'end');
    process.stdin.resume();
    const { worker, lockToken } = launch('interop-owner', directory);
    try {
        await completion(worker, 'ready');
        process.stdout.write('READY\n');
        await eof;
        const done = completion(worker);
        worker.postMessage('close');
        await done;
    } finally {
        await worker.terminate();
        releaseNodeStorageLease(directory, lockToken);
    }
} else {
    fs.mkdirSync(temporaryRoot, { recursive: true });
    const output = fs.mkdtempSync(join(temporaryRoot, 'node-storage-'));
    const directory = (name) => join(output, name);
    test.after(() => {
        assert.ok(resolve(output).startsWith(`${temporaryRoot}${sep}`));
        fs.rmSync(output, { recursive: true, force: true });
    });

    test('refuses main-thread installation without changing globals or files', async () => {
        const previous = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
        await assert.rejects(installNodeStorage(directory('main')), /dedicated worker/);
        assert.deepEqual(Object.getOwnPropertyDescriptor(globalThis, 'navigator'), previous);
        assert.equal(fs.existsSync(directory('main')), false);
    });

    test('persists the existing OPFS protocol across dedicated workers and deletes it', async () => {
        await run('write', directory('reopen'));
        await run('reopen', directory('reopen'));
    });

    test('enforces access-handle exclusion and closed-handle state', () => run('handles', directory('handles')));
    test('mounts the common native layout and preserves generation publication and cleanup', () =>
        run('mounted-generations', directory('mounted')));
    test('releases every acquired file after a partial session open without overwriting data', () =>
        run('partial-open', directory('partial')));
    test('propagates real filesystem failures and leaves stored data intact', () =>
        run('filesystem-errors', directory('errors')));
    test('cleans a failed global installation and a failed lease flush', async () => {
        await run('install-failure', directory('global-failure'));
        await run('metadata-failure', directory('metadata-failure'));
    });

    test('rejects unsafe names, directory links and hard links', async () => {
        const storageDirectory = directory('links');
        const outside = directory('outside');
        fs.mkdirSync(storageDirectory);
        fs.mkdirSync(outside);
        const bytes = new Uint8Array([41, 43]);
        fs.writeFileSync(join(outside, 'target.bin'), bytes);
        fs.symlinkSync(outside, join(storageDirectory, 'linked'), process.platform === 'win32' ? 'junction' : 'dir');
        fs.linkSync(join(outside, 'target.bin'), join(storageDirectory, 'linked.bin'));
        await run('names-and-links', storageDirectory);
        assert.deepEqual(fs.readFileSync(join(outside, 'target.bin')), Buffer.from(bytes));
    });

    test('excludes other workers and conditional cleanup cannot release another token', async () => {
        const storageDirectory = directory('exclusive');
        const { worker, lockToken } = launch('hold', storageDirectory);
        try {
            await completion(worker, 'ready');
            assert.equal(releaseNodeStorageLease(storageDirectory, randomUUID()), false);
            await run('locked', storageDirectory);
            const done = completion(worker);
            worker.postMessage('close');
            await done;
        } finally {
            await worker.terminate();
            releaseNodeStorageLease(storageDirectory, lockToken);
        }
        await run('reopen', storageDirectory);
    });

    test('releases matching leases after termination at each acquisition stage', async () => {
        for (const action of [
            'pause-before-record',
            'pause-before-publish',
            'pause-after-publish',
            'pause-after-unlink'
        ]) {
            const storageDirectory = directory(action);
            const { worker, lockToken } = launch(action, storageDirectory);
            try {
                await completion(worker, 'ready');
            } finally {
                await worker.terminate();
                assert.equal(releaseNodeStorageLease(storageDirectory, lockToken), true);
            }
            await run('write', storageDirectory);
        }
    });

    test('recovers an empty lease left by interrupted release', async () => {
        const storageDirectory = directory('empty-lease');
        fs.mkdirSync(storageDirectory);
        fs.mkdirSync(lockDirectory(storageDirectory));
        await run('write', storageDirectory);
    });

    test('excludes a different process and recovers a demonstrably dead process owner', async () => {
        const storageDirectory = directory('process');
        const child = spawn(process.execPath, [script, '--process-owner', storageDirectory], {
            stdio: ['ignore', 'pipe', 'pipe']
        });
        let stderr = '';
        child.stderr.on('data', (data) => {
            stderr += data;
        });
        try {
            await new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(new Error(`process owner did not start: ${stderr}`)), 10_000);
                child.stdout.once('data', () => {
                    clearTimeout(timer);
                    resolve();
                });
                child.once('error', (error) => {
                    clearTimeout(timer);
                    reject(error);
                });
                child.once('exit', (code) => {
                    clearTimeout(timer);
                    reject(new Error(`process owner exited: ${code} ${stderr}`));
                });
            });
            await run('locked', storageDirectory);
        } finally {
            const exited = once(child, 'exit');
            child.kill('SIGKILL');
            await exited;
        }
        await run('reopen', storageDirectory);
    });

    test('does not mutate a storage target that is a file', async () => {
        const path = directory('existing-file');
        fs.writeFileSync(path, new Uint8Array([47]));
        await run('invalid-path', path);
        assert.deepEqual([...fs.readFileSync(path)], [47]);
    });
}
