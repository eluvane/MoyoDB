// IO and control-file publication tests against an in-memory OPFS.
// Covers the failure modes of the original report: short writes accepted as
// success, a torn newer slot masking the valid one, and a corrupt control file
// being treated like a missing one.
import assert from 'node:assert/strict';
import { beforeEach, describe, test } from 'node:test';
import { setImmediate as nextTurn } from 'node:timers/promises';

const CONTROL_FILE_NAME = 'root-manifest.bin';
const CONTROL_SLOT_SIZE = 4096;
const CONTROL_MAGIC = new Uint8Array([66, 68, 66, 82, 79, 79, 84, 49]);

class MemoryFile {
    bytes = new Uint8Array(0);
    /** Replaces read; receives (dst, at, file) and returns the byte count. */
    readHook = null;
    reads = [];
    /** Replaces the default write; receives (src, at, file) and returns the byte count. */
    writeHook = null;
}

function loadBytes(file, dst, at, limit = dst.length) {
    const count = Math.max(0, Math.min(limit, dst.length, file.bytes.length - at));
    dst.set(file.bytes.subarray(at, at + count));
    return count;
}

function storeBytes(file, src, at) {
    const end = at + src.length;
    if (end > file.bytes.length) {
        const grown = new Uint8Array(end);
        grown.set(file.bytes);
        file.bytes = grown;
    }
    file.bytes.set(src, at);
    return src.length;
}

class MemoryAccessHandle {
    constructor(file) {
        this.file = file;
    }
    getSize() {
        return this.file.bytes.length;
    }
    read(dst, { at = 0 } = {}) {
        this.file.reads.push({ at, buffer: dst.buffer, byteOffset: dst.byteOffset, length: dst.length });
        return this.file.readHook ? this.file.readHook(dst, at, this.file) : loadBytes(this.file, dst, at);
    }
    write(src, { at = 0 } = {}) {
        return this.file.writeHook ? this.file.writeHook(src, at, this.file) : storeBytes(this.file, src, at);
    }
    truncate(size) {
        this.file.bytes = this.file.bytes.slice(0, size);
    }
    flush() {}
    close() {}
}

function notFound(name) {
    return new DOMException(`${name} not found`, 'NotFoundError');
}

class MemoryFileHandle {
    kind = 'file';
    constructor(name, file) {
        this.name = name;
        this.file = file;
    }
    async createSyncAccessHandle() {
        return new MemoryAccessHandle(this.file);
    }
    async getFile() {
        return { size: this.file.bytes.length };
    }
}

class MemoryDirectoryHandle {
    kind = 'directory';
    children = new Map();
    constructor(name) {
        this.name = name;
    }
    async getDirectoryHandle(name, { create = false } = {}) {
        const existing = this.children.get(name);
        if (existing?.kind === 'directory') {
            return existing;
        }
        if (existing || !create) {
            throw existing ? new DOMException(name, 'TypeMismatchError') : notFound(name);
        }
        const created = new MemoryDirectoryHandle(name);
        this.children.set(name, created);
        return created;
    }
    async getFileHandle(name, { create = false } = {}) {
        const existing = this.children.get(name);
        if (existing?.kind === 'file') {
            return existing;
        }
        if (existing || !create) {
            throw existing ? new DOMException(name, 'TypeMismatchError') : notFound(name);
        }
        const created = new MemoryFileHandle(name, new MemoryFile());
        this.children.set(name, created);
        return created;
    }
    async removeEntry(name) {
        if (!this.children.delete(name)) {
            throw notFound(name);
        }
    }
    async *entries() {
        yield* this.children.entries();
    }
}

const opfsRoot = new MemoryDirectoryHandle('');
Object.defineProperty(globalThis, 'navigator', {
    value: { storage: { getDirectory: async () => opfsRoot } },
    configurable: true,
    writable: true
});

const opfs = await import('./opfs_shim.js');
const {
    decodeControlSlot,
    encodeControlSlot,
    opfsCloseSession,
    opfsCleanupInactiveEntries,
    opfsOpenGenerationDb,
    opfsReadAt,
    opfsReadActiveGeneration,
    opfsSwapActiveGeneration,
    readControlStateFromAccessHandle,
    writeAll
} = opfs;

function controlBytes(slot0, slot1 = null) {
    const bytes = new Uint8Array(CONTROL_SLOT_SIZE * 2);
    if (slot0) {
        bytes.set(slot0, 0);
    }
    if (slot1) {
        bytes.set(slot1, CONTROL_SLOT_SIZE);
    }
    return bytes;
}

function handleOver(bytes) {
    const file = new MemoryFile();
    file.bytes = bytes;
    return new MemoryAccessHandle(file);
}

let dbCounter = 0;

async function seedDb({ control = null, generations = [], legacy = false } = {}) {
    dbCounter += 1;
    const name = `db-${dbCounter}`;
    const stackdb = await opfsRoot.getDirectoryHandle('stackdb', { create: true });
    const dbRoot = await stackdb.getDirectoryHandle(name, { create: true });
    await Promise.all(generations.map((generation) => dbRoot.getDirectoryHandle(generation, { create: true })));
    let controlFile = null;
    if (control) {
        controlFile = (await dbRoot.getFileHandle(CONTROL_FILE_NAME, { create: true })).file;
        controlFile.bytes = control;
    }
    if (legacy) {
        await dbRoot.getFileHandle('manifest.bin', { create: true });
    }
    return { name, dbRoot, controlFile };
}

async function openReadSession(bytes) {
    const generation = 'gen-read-1';
    const { name, dbRoot } = await seedDb({ generations: [generation] });
    const dir = await dbRoot.getDirectoryHandle(generation);
    const file = (await dir.getFileHandle('main.bin', { create: true })).file;
    file.bytes = bytes;
    const { sessionId } = await opfsOpenGenerationDb(name, generation, false);
    return { sessionId, file, name };
}

// Views do not own a new backing buffer. Count payload allocations and slice
// copies only during a synchronous operation, restoring both hooks afterwards.
function measureReadBuffers(operation) {
    const NativeUint8Array = globalThis.Uint8Array;
    const prototype = NativeUint8Array.prototype;
    const sliceDescriptor = Object.getOwnPropertyDescriptor(prototype, 'slice');
    const nativeSlice = prototype.slice;
    const work = { backingAllocations: 0, allocatedBytes: 0, slicedBytes: 0 };
    globalThis.Uint8Array = new Proxy(NativeUint8Array, {
        construct(target, args, newTarget) {
            const result = Reflect.construct(target, args, newTarget);
            const usesExistingBuffer =
                args[0] instanceof ArrayBuffer ||
                (typeof SharedArrayBuffer !== 'undefined' && args[0] instanceof SharedArrayBuffer);
            if (!usesExistingBuffer) {
                work.backingAllocations += 1;
                work.allocatedBytes += result.byteLength;
            }
            return result;
        }
    });
    Object.defineProperty(prototype, 'slice', {
        configurable: true,
        writable: true,
        value(...args) {
            const result = Reflect.apply(nativeSlice, this, args);
            work.slicedBytes += result.byteLength;
            return result;
        }
    });
    try {
        operation();
        return work;
    } finally {
        globalThis.Uint8Array = NativeUint8Array;
        if (sliceDescriptor) {
            Object.defineProperty(prototype, 'slice', sliceDescriptor);
        } else {
            delete prototype.slice;
        }
    }
}

function requireReadInto() {
    assert.equal(
        typeof opfs.opfsReadAtInto,
        'function',
        'read-into optimization is missing: opfsReadAtInto must fill a caller-owned buffer'
    );
    return opfs.opfsReadAtInto;
}

describe('OPFS reads', () => {
    test('keeps the requested size and zeroes bytes past EOF', async () => {
        const { sessionId, file } = await openReadSession(new Uint8Array([7, 8, 9]));
        try {
            assert.deepEqual(Array.from(opfsReadAt(sessionId, 1, 1n, 6)), [8, 9, 0, 0, 0, 0]);
            assert.deepEqual(
                file.reads.map(({ at, length }) => [at, length]),
                [
                    [1, 6],
                    [3, 4]
                ]
            );
            assert.deepEqual(Array.from(opfsReadAt(sessionId, 1, 64n, 3)), [0, 0, 0]);
            file.reads.length = 0;
            assert.equal(opfsReadAt(sessionId, 1, 0n, 0).length, 0);
            assert.equal(file.reads.length, 0);
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('retries short reads with the remaining range', async () => {
        const { sessionId, file } = await openReadSession(new Uint8Array([7, 8, 9, 10]));
        file.readHook = (dst, at, current) => loadBytes(current, dst, at, 1);
        try {
            assert.deepEqual(Array.from(opfsReadAt(sessionId, 1, 0n, 4)), [7, 8, 9, 10]);
            assert.deepEqual(
                file.reads.map(({ at, length }) => [at, length]),
                [
                    [0, 4],
                    [1, 3],
                    [2, 2],
                    [3, 1]
                ]
            );
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('rejects invalid read counts and unavailable handles', async () => {
        const { sessionId, file } = await openReadSession(new Uint8Array(4));
        try {
            for (const count of [-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, 5]) {
                file.readHook = () => count;
                assert.throws(() => opfsReadAt(sessionId, 1, 0n, 4), { name: 'StorageError' });
            }
            assert.throws(() => opfsReadAt(sessionId, 99, 0n, 0), /no OPFS access handle/);
            assert.throws(() => opfsReadAt(-1, 1, 0n, 0), /no OPFS session/);
        } finally {
            opfsCloseSession(sessionId);
        }
        assert.throws(() => opfsReadAt(sessionId, 1, 0n, 0), /no OPFS session/);
    });

    test('records the current allocating API payload work', async () => {
        const source = Uint8Array.from({ length: 2048 }, (_, index) => index & 0xff);
        const { sessionId, file } = await openReadSession(source);
        const outputs = [];
        try {
            const work = measureReadBuffers(() => {
                for (let index = 0; index < 16; index += 1) {
                    outputs.push(Array.from(opfsReadAt(sessionId, 1, BigInt(index * 64), 64)));
                }
            });
            assert.deepEqual(work, { backingAllocations: 16, allocatedBytes: 1024, slicedBytes: 0 });
            assert.equal(file.reads.length, 16);
            for (let index = 0; index < outputs.length; index += 1) {
                assert.deepEqual(outputs[index], Array.from(source.subarray(index * 64, (index + 1) * 64)));
            }
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('read-into passes the caller view to IO without payload allocations or copies', async () => {
        const readInto = requireReadInto();
        const source = Uint8Array.from({ length: 2048 }, (_, index) => index & 0xff);
        const { sessionId, file } = await openReadSession(source);
        const backing = new Uint8Array(80).fill(0xa5);
        const destination = backing.subarray(8, 72);
        const outputs = [];
        const counts = [];
        try {
            const work = measureReadBuffers(() => {
                for (let index = 0; index < 16; index += 1) {
                    counts.push(readInto(sessionId, 1, BigInt(index * 64), destination));
                    outputs.push(Array.from(destination));
                }
            });
            assert.deepEqual(work, { backingAllocations: 0, allocatedBytes: 0, slicedBytes: 0 });
            assert.deepEqual(counts, Array(16).fill(64));
            assert.equal(file.reads.length, 16);
            for (let index = 0; index < outputs.length; index += 1) {
                assert.deepEqual(outputs[index], Array.from(source.subarray(index * 64, (index + 1) * 64)));
                assert.equal(file.reads[index].buffer, destination.buffer);
                assert.equal(file.reads[index].byteOffset, destination.byteOffset);
                assert.equal(file.reads[index].length, destination.length);
            }
            assert.deepEqual(Array.from(backing.subarray(0, 8)), Array(8).fill(0xa5));
            assert.deepEqual(Array.from(backing.subarray(72)), Array(8).fill(0xa5));
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('read-into clears a reused destination only past EOF and respects its bounds', async () => {
        const readInto = requireReadInto();
        const { sessionId, file } = await openReadSession(new Uint8Array([7, 8, 9]));
        const backing = new Uint8Array(14).fill(0xa5);
        const destination = backing.subarray(2, 12);
        try {
            assert.equal(readInto(sessionId, 1, 1n, destination), 2);
            assert.deepEqual(Array.from(destination), [8, 9, 0, 0, 0, 0, 0, 0, 0, 0]);
            assert.deepEqual(
                file.reads.map(({ at, byteOffset, length }) => [at, byteOffset, length]),
                [
                    [1, 2, 10],
                    [3, 4, 8]
                ]
            );
            assert.deepEqual(Array.from(backing.subarray(0, 2)), [0xa5, 0xa5]);
            assert.deepEqual(Array.from(backing.subarray(12)), [0xa5, 0xa5]);
            destination.fill(0xcc);
            assert.equal(readInto(sessionId, 1, 64n, destination), 0);
            assert.deepEqual(Array.from(destination), Array(10).fill(0));
            file.reads.length = 0;
            assert.equal(readInto(sessionId, 1, 0n, destination.subarray(0, 0)), 0);
            assert.equal(file.reads.length, 0);
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('read-into retries short reads on the caller buffer and rejects invalid counts', async () => {
        const readInto = requireReadInto();
        const { sessionId, file } = await openReadSession(new Uint8Array([7, 8, 9, 10]));
        const destination = new Uint8Array(4);
        file.readHook = (dst, at, current) => loadBytes(current, dst, at, 1);
        try {
            assert.equal(readInto(sessionId, 1, 0n, destination), 4);
            assert.deepEqual(Array.from(destination), [7, 8, 9, 10]);
            assert.deepEqual(
                file.reads.map(({ at, byteOffset, length }) => [at, byteOffset, length]),
                [
                    [0, 0, 4],
                    [1, 1, 3],
                    [2, 2, 2],
                    [3, 3, 1]
                ]
            );
            assert.ok(file.reads.every(({ buffer }) => buffer === destination.buffer));
            for (const count of [-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, 5]) {
                file.readHook = () => count;
                assert.throws(() => readInto(sessionId, 1, 0n, destination), { name: 'StorageError' });
            }
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('read-into still validates sessions and file kinds for an empty destination', async () => {
        const readInto = requireReadInto();
        const { sessionId } = await openReadSession(new Uint8Array(0));
        const destination = new Uint8Array(0);
        try {
            assert.throws(() => readInto(sessionId, 99, 0n, destination), /no OPFS access handle/);
            assert.throws(() => readInto(-1, 1, 0n, destination), /no OPFS session/);
        } finally {
            opfsCloseSession(sessionId);
        }
        assert.throws(() => readInto(sessionId, 1, 0n, destination), /no OPFS session/);
    });
});

describe('writeAll', () => {
    test('rejects a write that makes no progress', () => {
        const handle = handleOver(new Uint8Array(8));
        handle.file.writeHook = () => 0;
        assert.throws(() => writeAll(handle, new Uint8Array([1, 2, 3]), 0), { name: 'StorageError' });
    });

    test('retries short writes until every byte is stored', () => {
        const handle = handleOver(new Uint8Array(0));
        handle.file.writeHook = (src, at, file) => storeBytes(file, src.subarray(0, 1), at);
        assert.equal(writeAll(handle, new Uint8Array([7, 8, 9]), 2), 3);
        assert.deepEqual(Array.from(handle.file.bytes), [0, 0, 7, 8, 9]);
    });
});

describe('control slots', () => {
    test('a torn newer slot does not hide the valid one', () => {
        const bytes = controlBytes(encodeControlSlot(1, 'gen-a-1'));
        bytes.set(CONTROL_MAGIC, CONTROL_SLOT_SIZE);
        assert.equal(decodeControlSlot(1, bytes.subarray(CONTROL_SLOT_SIZE)), null);
        const state = readControlStateFromAccessHandle(handleOver(bytes));
        assert.equal(state.status, 'valid');
        assert.equal(state.control.activeGeneration, 'gen-a-1');
    });

    test('the slot with the higher counter wins', () => {
        const bytes = controlBytes(encodeControlSlot(4, 'gen-a-1'), encodeControlSlot(5, 'gen-b-2'));
        const state = readControlStateFromAccessHandle(handleOver(bytes));
        assert.equal(state.control.activeGeneration, 'gen-b-2');
        assert.equal(state.control.slotIndex, 1);
    });

    test('a non-empty file without a valid slot is invalid, not absent', () => {
        assert.equal(readControlStateFromAccessHandle(handleOver(new Uint8Array(0))).status, 'absent');
        assert.equal(readControlStateFromAccessHandle(handleOver(new Uint8Array(8192))).status, 'invalid');
    });
});

describe('active generation', () => {
    test('a corrupt control file without legacy files is corruption', async () => {
        const { name } = await seedDb({ control: new Uint8Array(8192) });
        await assert.rejects(opfsReadActiveGeneration(name), { name: 'CorruptionError' });
    });

    test('a corrupt control file next to legacy files is a torn first publication', async () => {
        const { name } = await seedDb({ control: new Uint8Array(8192), legacy: true });
        assert.equal(await opfsReadActiveGeneration(name), null);
    });
});

describe('swapActiveGeneration', () => {
    let seeded;
    beforeEach(async () => {
        seeded = await seedDb({
            control: controlBytes(encodeControlSlot(1, 'gen-a-1')),
            generations: ['gen-a-1', 'gen-b-2']
        });
    });

    test('publishes into the other slot and reads it back', async () => {
        await opfsSwapActiveGeneration(seeded.name, 'gen-b-2', 'gen-a-1');
        assert.equal(await opfsReadActiveGeneration(seeded.name), 'gen-b-2');
        const state = readControlStateFromAccessHandle(handleOver(seeded.controlFile.bytes));
        assert.equal(state.control.slotIndex, 1);
        assert.equal(state.control.generationCounter, 2);
    });

    test('fails when the write stores nothing and keeps the old generation', async () => {
        seeded.controlFile.writeHook = () => 0;
        await assert.rejects(opfsSwapActiveGeneration(seeded.name, 'gen-b-2', 'gen-a-1'), { name: 'StorageError' });
        seeded.controlFile.writeHook = null;
        assert.equal(await opfsReadActiveGeneration(seeded.name), 'gen-a-1');
    });

    test('fails when the write is acknowledged but lost', async () => {
        seeded.controlFile.writeHook = (src) => src.length;
        await assert.rejects(opfsSwapActiveGeneration(seeded.name, 'gen-b-2', 'gen-a-1'), /does not select gen-b-2/);
        seeded.controlFile.writeHook = null;
        assert.equal(await opfsReadActiveGeneration(seeded.name), 'gen-a-1');
    });

    test('refuses to swap when the active generation is not the expected one', async () => {
        await assert.rejects(opfsSwapActiveGeneration(seeded.name, 'gen-b-2', 'gen-x-9'), {
            name: 'DatabaseBusyError'
        });
        await assert.rejects(opfsSwapActiveGeneration(seeded.name, 'gen-b-2', null), { name: 'DatabaseBusyError' });
        assert.equal(await opfsReadActiveGeneration(seeded.name), 'gen-a-1');
    });

    test('refuses to publish over a corrupt control file', async () => {
        const corrupt = await seedDb({ control: new Uint8Array(8192), generations: ['gen-b-2'] });
        await assert.rejects(opfsSwapActiveGeneration(corrupt.name, 'gen-b-2', null), { name: 'CorruptionError' });
    });

    test('first publication starts at slot 0 and cleanup keeps only the active generation', async () => {
        const fresh = await seedDb({ generations: ['gen-c-3', 'gen-d-4'], legacy: true });
        await opfsSwapActiveGeneration(fresh.name, 'gen-c-3', null);
        assert.equal(await opfsReadActiveGeneration(fresh.name), 'gen-c-3');
        await opfsCleanupInactiveEntries(fresh.name);
        assert.deepEqual(Array.from(fresh.dbRoot.children.keys()).toSorted(), ['gen-c-3', CONTROL_FILE_NAME]);
    });
});

// Resolve one wave of independent native calls at a time, in reverse order.
// This measures dependencies, not browser latency or filesystem throughput.
function queuedOpenIo(t, dir, { failStage, failKind, failure, unsupportedKind, closeFailureKind } = {}) {
    const names = ['manifest.bin', 'main.bin', 'wal.bin'];
    const getFile = dir.getFileHandle.bind(dir);
    const work = { calls: [], waves: [], closed: [], live: new Set() };
    let queued = [];
    function defer(label, action) {
        work.calls.push(label);
        return new Promise((resolve, reject) => {
            queued.push({
                label,
                run() {
                    try {
                        resolve(action());
                    } catch (error) {
                        reject(error);
                    }
                }
            });
        });
    }
    t.mock.method(dir, 'getFileHandle', (name, options) => {
        const kind = names.indexOf(name);
        if (kind < 0 || !options?.create) {
            return getFile(name, options);
        }
        return defer(`file:${kind}`, async () => {
            if (failStage === 'file' && failKind === kind) throw failure;
            const file = await getFile(name, options);
            if (unsupportedKind === kind) return {};
            return {
                createSyncAccessHandle() {
                    return defer(`access:${kind}`, async () => {
                        if (failStage === 'access' && failKind === kind) throw failure;
                        const handle = await file.createSyncAccessHandle();
                        work.live.add(kind);
                        t.mock.method(handle, 'close', () => {
                            work.closed.push(kind);
                            work.live.delete(kind);
                            if (closeFailureKind === kind) throw new Error('injected close failure');
                        });
                        t.mock.method(handle, 'write', () => assert.fail('opening must not write data'));
                        t.mock.method(handle, 'flush', () => assert.fail('opening must not change flush semantics'));
                        t.mock.method(handle, 'truncate', () => assert.fail('opening must not truncate data'));
                        return handle;
                    });
                }
            };
        });
    });
    return {
        work,
        async finish(operation) {
            let outcome;
            operation.then(
                (value) => {
                    outcome = { ok: true, value };
                },
                (error) => {
                    outcome = { ok: false, error };
                }
            );
            async function drainTurn(turn) {
                if (outcome !== undefined || turn === 32) return;
                await nextTurn();
                const ready = queued;
                queued = [];
                if (ready.length > 0) work.waves.push(ready.map(({ label }) => label));
                for (const item of ready.toReversed()) item.run();
                await drainTurn(turn + 1);
            }
            await drainTurn(0);
            assert.notEqual(outcome, undefined, 'open failed to settle after draining native calls');
            assert.equal(queued.length, 0, 'open returned with native calls still pending');
            if (!outcome.ok) throw outcome.error;
            return outcome.value;
        }
    };
}

describe('OPFS open work and failure cleanup', () => {
    test('creates a missing DB with one directory request and preserves existing files', async (t) => {
        const stackdb = await opfsRoot.getDirectoryHandle('stackdb', { create: true });
        const getDirectory = stackdb.getDirectoryHandle.bind(stackdb);
        const name = `open-new-${++dbCounter}`;
        const calls = [];
        t.mock.method(stackdb, 'getDirectoryHandle', (entry, options) => {
            if (entry === name) calls.push(options.create);
            return getDirectory(entry, options);
        });
        let session = await opfs.opfsOpenActiveDb(name);
        try {
            assert.deepEqual(calls, [true]);
            opfs.opfsWriteAt(session.sessionId, 1, 0n, new Uint8Array([9, 8, 7]));
        } finally {
            opfsCloseSession(session.sessionId);
        }
        calls.length = 0;
        session = await opfs.opfsOpenActiveDb(name);
        try {
            assert.deepEqual(calls, [true]);
            assert.deepEqual(Array.from(opfsReadAt(session.sessionId, 1, 0n, 3)), [9, 8, 7]);
        } finally {
            opfsCloseSession(session.sessionId);
        }
    });

    test('does not create a missing DB when createIfMissing is false', async () => {
        const stackdb = await opfsRoot.getDirectoryHandle('stackdb', { create: true });
        const name = `open-missing-${++dbCounter}`;
        await assert.rejects(opfs.opfsOpenActiveDb(name, false), { message: `database ${name} does not exist` });
        assert.equal(stackdb.children.has(name), false);
        const collision = await stackdb.getFileHandle(name, { create: true });
        await assert.rejects(opfs.opfsOpenActiveDb(name, true), { name: 'TypeMismatchError' });
        await assert.rejects(opfs.opfsOpenActiveDb(name, false), { message: `database ${name} does not exist` });
        assert.equal(stackdb.children.get(name), collision);
    });

    test('opens three files in two dependency waves with stable file-kind mapping', async (t) => {
        const { name, dbRoot } = await seedDb();
        const names = ['manifest.bin', 'main.bin', 'wal.bin'];
        await Promise.all(
            names.map(async (entry, kind) => {
                const file = await dbRoot.getFileHandle(entry, { create: true });
                file.file.bytes = new Uint8Array([kind + 1]);
            })
        );
        const io = queuedOpenIo(t, dbRoot);
        const { sessionId, generationName } = await io.finish(opfs.opfsOpenActiveDb(name, false));
        try {
            t.diagnostic(`file acquisitions: ${io.work.calls.length}; dependency waves: ${io.work.waves.length}`);
            assert.equal(generationName, null);
            assert.equal(io.work.calls.length, 6);
            assert.equal(io.work.waves.length, 2);
            assert.deepEqual(
                io.work.waves.map((wave) => wave.toSorted()),
                [
                    ['file:0', 'file:1', 'file:2'],
                    ['access:0', 'access:1', 'access:2']
                ]
            );
            assert.equal(io.work.live.size, 3);
            for (let kind = 0; kind < names.length; kind += 1) {
                assert.deepEqual(Array.from(opfsReadAt(sessionId, kind, 0n, 1)), [kind + 1]);
            }
        } finally {
            opfsCloseSession(sessionId);
        }
        assert.deepEqual(io.work.closed, [0, 1, 2]);
        assert.equal(io.work.live.size, 0);
    });

    for (const failStage of ['file', 'access']) {
        for (const failKind of [0, 1, 2]) {
            test(`drains and closes successful opens after ${failStage} failure at kind ${failKind}`, async (t) => {
                const { name, dbRoot } = await seedDb();
                const failure = new DOMException('injected acquisition failure', 'NotAllowedError');
                const io = queuedOpenIo(t, dbRoot, { failStage, failKind, failure });
                await assert.rejects(io.finish(opfs.opfsOpenActiveDb(name, false)), (error) => error === failure);
                assert.equal(io.work.live.size, 0);
                assert.deepEqual(
                    io.work.closed,
                    [0, 1, 2].filter((kind) => kind !== failKind)
                );
                assert.equal(io.work.calls.filter((call) => call.startsWith('file:')).length, 3);
            });
        }
    }

    test('closes other handles when SyncAccessHandle is unavailable', async (t) => {
        const { name, dbRoot } = await seedDb();
        const io = queuedOpenIo(t, dbRoot, { unsupportedKind: 1 });
        await assert.rejects(io.finish(opfs.opfsOpenActiveDb(name, false)), /createSyncAccessHandle is unavailable/);
        assert.deepEqual(io.work.closed, [0, 2]);
        assert.equal(io.work.live.size, 0);
    });

    test('attempts every close and preserves even a falsy acquisition error', async (t) => {
        const { name, dbRoot } = await seedDb();
        const io = queuedOpenIo(t, dbRoot, {
            failStage: 'access',
            failKind: 1,
            failure: undefined,
            closeFailureKind: 0
        });
        let rejected = false;
        try {
            await io.finish(opfs.opfsOpenActiveDb(name, false));
        } catch (error) {
            rejected = true;
            assert.equal(error, undefined);
        }
        assert.equal(rejected, true);
        assert.deepEqual(io.work.closed, [0, 2]);
    });

    test('waits for late handles before rejection, closes them, and does not publish a session', async (t) => {
        const { name, dbRoot } = await seedDb();
        const before = await opfs.opfsOpenActiveDb(name, false);
        opfsCloseSession(before.sessionId);
        const names = ['manifest.bin', 'main.bin', 'wal.bin'];
        const gates = names.map(() => Promise.withResolvers());
        const closed = [];
        const started = [];
        const handles = await Promise.all(
            names.map(async (entry, kind) => {
                const file = await dbRoot.getFileHandle(entry);
                const handle = await file.createSyncAccessHandle();
                t.mock.method(handle, 'close', () => closed.push(kind));
                t.mock.method(file, 'createSyncAccessHandle', () => {
                    started.push(kind);
                    return gates[kind].promise;
                });
                return handle;
            })
        );
        const failure = new Error('middle file failed');
        let settled = false;
        const opening = opfs.opfsOpenActiveDb(name, false);
        const rejected = assert.rejects(opening, (error) => error === failure);
        opening.then(
            () => {
                settled = true;
            },
            () => {
                settled = true;
            }
        );
        await nextTurn();
        assert.deepEqual(started, [0, 1, 2]);
        gates[0].resolve(handles[0]);
        gates[1].reject(failure);
        await nextTurn();
        assert.equal(settled, false, 'failure escaped while the WAL handle was still pending');
        assert.deepEqual(closed, []);
        assert.throws(() => opfs.opfsLen(before.sessionId + 1, 0), /no OPFS session/);
        gates[2].resolve(handles[2]);
        await rejected;
        assert.deepEqual(closed, [0, 2]);
        t.mock.restoreAll();
        const reopened = await opfs.opfsOpenActiveDb(name, false);
        try {
            assert.equal(reopened.sessionId, before.sessionId + 1);
        } finally {
            opfsCloseSession(reopened.sessionId);
        }
    });

    test('reports the first file-order failure rather than the fastest failure', async (t) => {
        const { name, dbRoot } = await seedDb();
        const gates = [Promise.withResolvers(), Promise.withResolvers()];
        const first = new Error('manifest failed later');
        const last = new Error('WAL failed first');
        const getFile = dbRoot.getFileHandle.bind(dbRoot);
        let closed = false;
        t.mock.method(dbRoot, 'getFileHandle', async (entry, options) => {
            if (entry === 'manifest.bin') return gates[0].promise;
            if (entry === 'wal.bin') return gates[1].promise;
            const file = await getFile(entry, options);
            if (entry === 'main.bin') {
                t.mock.method(file, 'createSyncAccessHandle', async () => {
                    const handle = new MemoryAccessHandle(file.file);
                    t.mock.method(handle, 'close', () => {
                        closed = true;
                    });
                    return handle;
                });
            }
            return file;
        });
        const opening = opfs.opfsOpenActiveDb(name, false);
        const rejected = assert.rejects(opening, (error) => error === first);
        await nextTurn();
        gates[1].reject(last);
        await nextTurn();
        gates[0].reject(first);
        await rejected;
        assert.equal(closed, true);
    });

    test('a legacy-cleanup failure prevents data-handle acquisition', async (t) => {
        const generation = 'gen-open-2';
        const { name, dbRoot } = await seedDb({
            control: controlBytes(encodeControlSlot(1, generation)),
            generations: [generation],
            legacy: true
        });
        const io = queuedOpenIo(t, await dbRoot.getDirectoryHandle(generation));
        const failure = new DOMException('legacy cleanup failed', 'NotAllowedError');
        t.mock.method(dbRoot, 'removeEntry', async () => {
            throw failure;
        });
        await assert.rejects(io.finish(opfs.opfsOpenActiveDb(name, false)), (error) => error === failure);
        assert.deepEqual(io.work.calls, []);
    });

    test('does not open data files before corrupt control state is rejected', async (t) => {
        const { name, dbRoot } = await seedDb({ control: new Uint8Array(8192) });
        const io = queuedOpenIo(t, dbRoot);
        await assert.rejects(io.finish(opfs.opfsOpenActiveDb(name, false)), { name: 'CorruptionError' });
        assert.deepEqual(io.work.calls, []);
        assert.deepEqual([...dbRoot.children.keys()], [CONTROL_FILE_NAME]);
    });

    test('published-generation open removes legacy files before acquiring data handles', async (t) => {
        const generation = 'gen-open-1';
        const { name, dbRoot } = await seedDb({
            control: controlBytes(encodeControlSlot(1, generation)),
            generations: [generation],
            legacy: true
        });
        const dir = await dbRoot.getDirectoryHandle(generation);
        const io = queuedOpenIo(t, dir);
        const { sessionId, generationName } = await io.finish(opfs.opfsOpenActiveDb(name, false));
        try {
            assert.equal(generationName, generation);
            assert.equal(dbRoot.children.has('manifest.bin'), false);
            assert.deepEqual([...dir.children.keys()].toSorted(), ['main.bin', 'manifest.bin', 'wal.bin']);
            assert.equal(io.work.waves.length, 2);
        } finally {
            opfsCloseSession(sessionId);
        }
    });
});

describe('OPFS serial storage operations', () => {
    test('legacy detection stops at the first existing file', async (t) => {
        const { name, dbRoot } = await seedDb({ control: new Uint8Array(8192), legacy: true });
        const getFile = dbRoot.getFileHandle.bind(dbRoot);
        const calls = [];
        t.mock.method(dbRoot, 'getFileHandle', (entry, options) => {
            calls.push(entry);
            if (entry === 'main.bin' || entry === 'wal.bin') {
                throw new DOMException('must not inspect later legacy files', 'NotAllowedError');
            }
            return getFile(entry, options);
        });
        assert.equal(await opfsReadActiveGeneration(name), null);
        assert.deepEqual(calls, [CONTROL_FILE_NAME, 'manifest.bin']);
    });

    test('legacy detection propagates lookup errors before inspecting later files', async (t) => {
        const { name, dbRoot } = await seedDb({ control: new Uint8Array(8192) });
        const getFile = dbRoot.getFileHandle.bind(dbRoot);
        const failure = new DOMException('main lookup denied', 'NotAllowedError');
        const calls = [];
        t.mock.method(dbRoot, 'getFileHandle', (entry, options) => {
            calls.push(entry);
            if (entry === 'main.bin') throw failure;
            return getFile(entry, options);
        });
        await assert.rejects(opfsReadActiveGeneration(name), (error) => error === failure);
        assert.deepEqual(calls, [CONTROL_FILE_NAME, 'manifest.bin', 'main.bin']);
    });

    test('legacy cleanup stops on failure before later deletions or data opens', async (t) => {
        const generation = 'gen-serial-1';
        const { name, dbRoot } = await seedDb({
            control: controlBytes(encodeControlSlot(1, generation)),
            generations: [generation],
            legacy: true
        });
        await Promise.all(['main.bin', 'wal.bin'].map((entry) => dbRoot.getFileHandle(entry, { create: true })));
        const io = queuedOpenIo(t, await dbRoot.getDirectoryHandle(generation));
        const remove = dbRoot.removeEntry.bind(dbRoot);
        const failure = new DOMException('main deletion denied', 'NotAllowedError');
        const calls = [];
        t.mock.method(dbRoot, 'removeEntry', (entry, options) => {
            calls.push(entry);
            if (entry === 'main.bin') throw failure;
            return remove(entry, options);
        });
        await assert.rejects(io.finish(opfs.opfsOpenActiveDb(name, false)), (error) => error === failure);
        assert.deepEqual(calls, ['manifest.bin', 'main.bin']);
        assert.equal(dbRoot.children.has('manifest.bin'), false);
        assert.equal(dbRoot.children.has('main.bin'), true);
        assert.equal(dbRoot.children.has('wal.bin'), true);
        assert.deepEqual(io.work.calls, []);
    });

    test('stale cleanup waits for each deletion and continues after a failure', async (t) => {
        const active = 'gen-serial-1';
        const stale = ['gen-serial-2', 'gen-serial-3'];
        const { name, dbRoot } = await seedDb({
            control: controlBytes(encodeControlSlot(1, active)),
            generations: [active, ...stale, 'unrelated']
        });
        const remove = dbRoot.removeEntry.bind(dbRoot);
        const first = Promise.withResolvers();
        const started = [];
        t.mock.method(dbRoot, 'removeEntry', (entry, options) => {
            if (!stale.includes(entry)) return remove(entry, options);
            assert.deepEqual(options, { recursive: true });
            started.push(entry);
            return entry === stale[0] ? first.promise : remove(entry, options);
        });
        const cleanup = opfsCleanupInactiveEntries(name);
        await nextTurn();
        assert.deepEqual(started, [stale[0]]);
        first.reject(new DOMException('stale deletion denied', 'NotAllowedError'));
        await cleanup;
        assert.deepEqual(started, stale);
        assert.deepEqual([...dbRoot.children.keys()].toSorted(), [active, stale[0], CONTROL_FILE_NAME, 'unrelated']);
    });

    test('rebuild preparation retries a name collision without modifying the existing generation', async (t) => {
        const { name, dbRoot } = await seedDb({ generations: ['gen-ya-01010101'] });
        const existing = await dbRoot.getDirectoryHandle('gen-ya-01010101');
        const file = await existing.getFileHandle('main.bin', { create: true });
        file.file.bytes = new Uint8Array([9, 8, 7]);
        let attempts = 0;
        t.mock.method(Date, 'now', () => 1234);
        t.mock.method(crypto, 'getRandomValues', (bytes) => bytes.fill(++attempts));
        const result = await opfs.opfsPrepareRebuildTarget(name);
        assert.deepEqual(result, { generationName: 'gen-ya-02020202' });
        assert.equal(attempts, 2);
        assert.deepEqual([...dbRoot.children.keys()], ['gen-ya-01010101', 'gen-ya-02020202']);
        assert.deepEqual(Array.from(file.file.bytes), [9, 8, 7]);
    });
});

// These are shim work counts, not physical disk IOPS or browser timings.
describe('prebatched OPFS write work', () => {
    test('keeps each prebuilt buffer in one storage call without copying its payload', (t) => {
        const sizes = [0, 4096, 65536, 262144, 262145, 1048576, 8388608, 67108864];
        const backing = new Uint8Array(sizes.at(-1) + 32);
        for (let index = 0; index < backing.length; index += 1) {
            backing[index] = (index * 37) ^ (index >>> 8);
        }
        const results = [];
        for (const length of sizes) {
            const source = backing.subarray(11, 11 + length);
            const handle = handleOver(new Uint8Array(length + 34).fill(0xa5));
            let calls = 0;
            handle.file.writeHook = (src, at, file) => {
                assert.equal(src.buffer, source.buffer, 'the shim must borrow, not copy, the source');
                assert.equal(src.byteOffset, source.byteOffset + at - 17);
                assert.ok(at >= 17 && at + src.length <= 17 + length);
                calls += 1;
                return storeBytes(file, src, at);
            };
            const work = measureReadBuffers(() => {
                assert.equal(writeAll(handle, source, 17), length);
            });
            assert.deepEqual(work, { backingAllocations: 0, allocatedBytes: 0, slicedBytes: 0 });
            assert.deepEqual(handle.file.bytes.subarray(17, 17 + length), source);
            assert.ok(handle.file.bytes.subarray(0, 17).every((byte) => byte === 0xa5));
            assert.ok(handle.file.bytes.subarray(17 + length).every((byte) => byte === 0xa5));
            results.push({ bytes: length, writeCalls: calls });
        }
        t.diagnostic(JSON.stringify(results));
        assert.deepEqual(
            results.map(({ writeCalls }) => writeCalls),
            sizes.map((length) => (length === 0 ? 0 : 1))
        );
    });

    test('splits only at the browser byte-count limit, including after short writes', () => {
        const maxWrite = 0x7fffffff;
        for (const length of [maxWrite - 1, maxWrite, maxWrite + 1, maxWrite * 2 + 17]) {
            for (const shortFirst of [false, true]) {
                // Virtual spans test >2 GiB arithmetic without a multi-GiB allocation.
                // The real-buffer tests separately verify bytes and borrowed views.
                const source = {
                    length,
                    start: 0,
                    subarray(start, end) {
                        assert.ok(start >= 0 && end >= start && end <= length);
                        return { start, length: end - start };
                    }
                };
                const offered = [];
                let stored = 0;
                const handle = {
                    write(span, { at }) {
                        assert.equal(span.start, stored);
                        assert.equal(at, 19 + stored);
                        assert.equal(span.length, Math.min(maxWrite, length - stored));
                        offered.push(span.length);
                        const count = shortFirst && offered.length === 1 ? 7 : span.length;
                        stored += count;
                        return count;
                    }
                };
                assert.equal(writeAll(handle, source, 19), length);
                assert.equal(stored, length);
                assert.equal(
                    offered.length,
                    shortFirst ? 1 + Math.ceil((length - 7) / maxWrite) : Math.ceil(length / maxWrite)
                );
            }
        }
    });

    test('retries only the remaining suffix after native short writes', () => {
        const backing = new Uint8Array(1048576 + 43);
        for (let index = 0; index < backing.length; index += 1) backing[index] = index * 13;
        const source = backing.subarray(7, backing.length - 9);
        const handle = handleOver(new Uint8Array(source.length + 26).fill(0xa5));
        const accepted = [262145, 3, 65537];
        const offered = [];
        let stored = 0;
        handle.file.writeHook = (src, at, file) => {
            offered.push(src.length);
            assert.equal(at, 13 + stored);
            assert.equal(src.buffer, source.buffer);
            assert.equal(src.byteOffset, source.byteOffset + stored);
            const count = Math.min(src.length, accepted[offered.length - 1] ?? src.length);
            stored += count;
            return storeBytes(file, src.subarray(0, count), at);
        };
        assert.equal(writeAll(handle, source, 13), source.length);
        assert.deepEqual(offered, [
            source.length,
            source.length - 262145,
            source.length - 262148,
            source.length - 327685
        ]);
        assert.deepEqual(handle.file.bytes.subarray(13, 13 + source.length), source);
        assert.ok(handle.file.bytes.subarray(0, 13).every((byte) => byte === 0xa5));
        assert.ok(handle.file.bytes.subarray(13 + source.length).every((byte) => byte === 0xa5));
    });

    test('validates every returned count against the offered suffix and stops on failure', () => {
        for (const prefix of [0, 5]) {
            for (const invalid of [0, -1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, 9 - prefix]) {
                let calls = 0;
                const handle = {
                    write() {
                        calls += 1;
                        return prefix > 0 && calls === 1 ? prefix : invalid;
                    }
                };
                assert.throws(() => writeAll(handle, new Uint8Array(8), 91), {
                    name: 'StorageError',
                    code: 'StorageError'
                });
                assert.equal(calls, prefix > 0 ? 2 : 1);
            }
        }
    });

    test('propagates native storage errors unchanged after partial progress', () => {
        for (const failure of [new DOMException('quota exhausted', 'QuotaExceededError'), new Error('write failed')]) {
            const handle = handleOver(new Uint8Array(12).fill(0xa5));
            let calls = 0;
            handle.file.writeHook = (src, at, file) => {
                calls += 1;
                if (calls === 2) throw failure;
                return storeBytes(file, src.subarray(0, 3), at);
            };
            assert.throws(
                () => writeAll(handle, new Uint8Array([7, 8, 9, 10, 11]), 2),
                (error) => error === failure
            );
            assert.equal(calls, 2);
            assert.deepEqual(
                Array.from(handle.file.bytes),
                [0xa5, 0xa5, 7, 8, 9, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5]
            );
        }
    });

    test('uses the current WASM memory view after growth without retaining the old one', () => {
        const memory = new WebAssembly.Memory({ initial: 5, maximum: 16 });
        for (const pages of [0, 5]) {
            memory.grow(pages);
            const source = new Uint8Array(memory.buffer, 37, memory.buffer.byteLength - 74);
            source.fill(0x6d);
            const handle = handleOver(new Uint8Array(source.length));
            handle.file.writeHook = (src, at, file) => {
                assert.equal(src.buffer, memory.buffer);
                return storeBytes(file, src, at);
            };
            assert.equal(writeAll(handle, source, 0), source.length);
            assert.deepEqual(handle.file.bytes, source);
        }
    });

    test('does not merge logical writes or add, remove, or defer caller flushes', async (t) => {
        const { name, dbRoot } = await seedDb();
        const { sessionId } = await opfs.opfsOpenActiveDb(name, false);
        const wal = (await dbRoot.getFileHandle('wal.bin')).file;
        const events = [];
        t.mock.method(MemoryAccessHandle.prototype, 'flush', function () {
            assert.equal(this.file, wal);
            events.push({ kind: 'flush' });
        });
        wal.writeHook = (src, at, file) => {
            events.push({ kind: 'write', at, length: src.length });
            return storeBytes(file, src, at);
        };
        const bytes = new Uint8Array(1048576).fill(0x39);
        try {
            assert.equal(opfs.opfsWriteAt(sessionId, 2, 0n, bytes), bytes.length);
            opfs.opfsFlush(sessionId, 2);
            assert.equal(opfs.opfsWriteAt(sessionId, 2, BigInt(bytes.length), bytes), bytes.length);
            opfs.opfsFlush(sessionId, 2);
            assert.deepEqual(events, [
                { kind: 'write', at: 0, length: bytes.length },
                { kind: 'flush' },
                { kind: 'write', at: bytes.length, length: bytes.length },
                { kind: 'flush' }
            ]);
            assert.equal(opfs.opfsLen(sessionId, 2), BigInt(bytes.length * 2));
            assert.deepEqual(wal.bytes.subarray(0, bytes.length), bytes);
            assert.deepEqual(wal.bytes.subarray(bytes.length), bytes);
            assert.throws(() => opfs.opfsWriteAt(sessionId, 99, 0n, new Uint8Array()), /no OPFS access handle/);
        } finally {
            opfsCloseSession(sessionId);
        }
        assert.throws(() => opfs.opfsWriteAt(sessionId, 2, 0n, new Uint8Array()), /no OPFS session/);
    });
});

describe('private OPFS append offsets', () => {
    test('shares append state across backend aliases without repeated size queries or deferred flushes', async (t) => {
        const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize');
        const flushes = t.mock.method(MemoryAccessHandle.prototype, 'flush');
        for (const commits of [1, 10, 1000]) {
            const { sessionId, file } = await openReadSession(new Uint8Array());
            // Rust OpfsBackend clones carry the same session/file-kind pair.
            const aliases = [0, 1].map(() => ({
                append: () => opfs.opfsAppendOffset(sessionId, 1),
                write: (at, bytes) => opfs.opfsWriteAt(sessionId, 1, at, bytes)
            }));
            const sizeCalls = sizes.mock.callCount();
            const flushCalls = flushes.mock.callCount();
            try {
                assert.equal(opfs.opfsLen(sessionId, 1), 0n);
                for (let index = 0; index < commits; index += 1) {
                    const alias = aliases[index % aliases.length];
                    assert.equal(alias.append(), BigInt(index * 3));
                    assert.equal(alias.write(alias.append(), new Uint8Array([index % 251, 7, 9])), 3);
                    opfs.opfsFlush(sessionId, 1);
                    assert.equal(flushes.mock.callCount() - flushCalls, index + 1);
                }
                assert.equal(aliases[0].append(), BigInt(commits * 3));
                assert.equal(sizes.mock.callCount() - sizeCalls, 1, 'only the authoritative startup read');
                assert.equal(file.bytes.length, commits * 3);
                for (let index = 0; index < commits; index += 1) {
                    assert.deepEqual([...file.bytes.subarray(index * 3, index * 3 + 3)], [index % 251, 7, 9]);
                }
                assert.equal(opfs.opfsAppendOffset(sessionId, 0), 0n, 'another file has its own state');
                assert.equal(sizes.mock.callCount() - sizeCalls, 2);
            } finally {
                opfsCloseSession(sessionId);
            }
            assert.throws(() => aliases[0].append(), /no OPFS session/);
        }
    });

    test('length and directory statistics still read actual sizes and refresh the append offset', async (t) => {
        const { sessionId, file, name } = await openReadSession(new Uint8Array(8));
        const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize');
        try {
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 8n);
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 8n);
            assert.equal(sizes.mock.callCount(), 1);
            assert.equal(opfs.opfsLen(sessionId, 1), 8n);
            file.bytes = new Uint8Array(13);
            assert.equal(opfs.opfsLen(sessionId, 1), 13n);
            assert.equal(sizes.mock.callCount(), 3);
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 13n);
            file.bytes = new Uint8Array(17);
            assert.equal(await opfs.opfsDbDirectorySize(name), 17);
            assert.equal(sizes.mock.callCount(), 6, 'directory stats read all three open files');
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 17n);
            assert.throws(() => opfs.opfsAppendOffset(sessionId, 99), /no OPFS access handle/);
        } finally {
            opfsCloseSession(sessionId);
        }
        file.bytes = new Uint8Array(19);
        const reopened = await opfsOpenGenerationDb(name, 'gen-read-1', false);
        try {
            assert.equal(opfs.opfsAppendOffset(reopened.sessionId, 1), 19n);
            assert.equal(sizes.mock.callCount(), 7, 'a reopened handle must observe the file');
        } finally {
            opfsCloseSession(reopened.sessionId);
        }
    });

    test('unknown overwrites stay unknown; empty writes never extend EOF; short writes update the complete end', async (t) => {
        const { sessionId, file } = await openReadSession(new Uint8Array(8));
        const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize');
        const writes = t.mock.method(MemoryAccessHandle.prototype, 'write');
        try {
            assert.equal(opfs.opfsWriteAt(sessionId, 1, 500n, new Uint8Array()), 0);
            assert.equal(writes.mock.callCount(), 0);
            assert.equal(opfs.opfsWriteAt(sessionId, 1, 1n, new Uint8Array([2, 3])), 2);
            assert.equal(sizes.mock.callCount(), 0, 'writes do not add size queries');
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 8n, 'an overwrite does not reveal the old EOF');
            file.writeHook = (src, at, current) => storeBytes(current, src.subarray(0, 2), at);
            assert.equal(opfs.opfsWriteAt(sessionId, 1, 2n, new Uint8Array([4, 5, 6])), 3);
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 8n, 'an overwrite preserves a longer file');
            assert.equal(opfs.opfsWriteAt(sessionId, 1, 11n, new Uint8Array([7, 8, 9, 10, 11])), 5);
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 16n);
            const writeCalls = writes.mock.callCount();
            assert.equal(opfs.opfsWriteAt(sessionId, 1, 1000n, new Uint8Array()), 0);
            assert.equal(writes.mock.callCount(), writeCalls);
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 16n);
            assert.equal(sizes.mock.callCount(), 1);
            assert.deepEqual([...file.bytes], [0, 2, 4, 5, 6, 0, 0, 0, 0, 0, 0, 7, 8, 9, 10, 11]);
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('successful truncate establishes EOF without a query, including growth and an unprimed handle', async (t) => {
        const { sessionId, file } = await openReadSession(new Uint8Array(8).fill(7));
        const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize');
        t.mock.method(MemoryAccessHandle.prototype, 'truncate', function (length) {
            const next = new Uint8Array(length);
            next.set(this.file.bytes.subarray(0, length));
            this.file.bytes = next;
        });
        try {
            for (const length of [3, 12, 0]) {
                opfs.opfsTruncate(sessionId, 1, BigInt(length));
                assert.equal(opfs.opfsAppendOffset(sessionId, 1), BigInt(length));
                assert.equal(file.bytes.length, length);
            }
            assert.equal(sizes.mock.callCount(), 0);
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('failed partial writes invalidate the offset and preserve even non-Error thrown values', async (t) => {
        const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize');
        for (const failure of [new Error('write failed'), undefined, null, 0, false]) {
            const { sessionId, file } = await openReadSession(new Uint8Array(4));
            let writes = 0;
            file.writeHook = (src, at, current) => {
                if (++writes === 2) throw failure;
                return storeBytes(current, src.subarray(0, 2), at);
            };
            try {
                assert.equal(opfs.opfsAppendOffset(sessionId, 1), 4n);
                const sizeCalls = sizes.mock.callCount();
                assert.throws(
                    () => opfs.opfsWriteAt(sessionId, 1, 4n, new Uint8Array([8, 9, 10, 11])),
                    (error) => error === failure
                );
                assert.equal(opfs.opfsAppendOffset(sessionId, 1), 6n);
                assert.equal(sizes.mock.callCount(), sizeCalls + 1);
                assert.deepEqual([...file.bytes], [0, 0, 0, 0, 8, 9]);
            } finally {
                opfsCloseSession(sessionId);
            }
        }
    });

    test('invalid native write counts invalidate a previously observed offset', async (t) => {
        const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize');
        for (const invalid of [0, -1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, 5]) {
            const { sessionId, file } = await openReadSession(new Uint8Array(4));
            file.writeHook = (src, at, current) => {
                storeBytes(current, src.subarray(0, 1), at);
                return invalid;
            };
            try {
                assert.equal(opfs.opfsAppendOffset(sessionId, 1), 4n);
                const sizeCalls = sizes.mock.callCount();
                assert.throws(() => opfs.opfsWriteAt(sessionId, 1, 4n, new Uint8Array(4)), { name: 'StorageError' });
                assert.equal(opfs.opfsAppendOffset(sessionId, 1), 5n);
                assert.equal(sizes.mock.callCount(), sizeCalls + 1);
            } finally {
                opfsCloseSession(sessionId);
            }
        }
    });

    for (const operation of ['truncate', 'flush', 'getSize']) {
        test(`a failed ${operation} invalidates the offset before recovery and preserves the error`, async (t) => {
            const { sessionId, file } = await openReadSession(new Uint8Array(4));
            const failure = new Error(`${operation} failed`);
            try {
                assert.equal(opfs.opfsAppendOffset(sessionId, 1), 4n);
                const failing = t.mock.method(MemoryAccessHandle.prototype, operation, function () {
                    // A failed operation may have changed the file, or the
                    // browser may report an uncertain state requiring recovery.
                    this.file.bytes = new Uint8Array(7);
                    throw failure;
                });
                assert.throws(
                    () => {
                        if (operation === 'truncate') opfs.opfsTruncate(sessionId, 1, 7n);
                        else if (operation === 'flush') opfs.opfsFlush(sessionId, 1);
                        else opfs.opfsLen(sessionId, 1);
                    },
                    (error) => error === failure
                );
                failing.mock.restore();
                const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize');
                assert.equal(opfs.opfsAppendOffset(sessionId, 1), 7n);
                assert.equal(sizes.mock.callCount(), 1);
                assert.equal(file.bytes.length, 7);
            } finally {
                opfsCloseSession(sessionId);
            }
        });
    }

    test('directory-size failures invalidate the same private handle state', async (t) => {
        const { sessionId, file, name } = await openReadSession(new Uint8Array(4));
        const failure = new Error('directory size failed');
        try {
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 4n);
            const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize', function () {
                if (this.file === file) throw failure;
                return this.file.bytes.length;
            });
            await assert.rejects(opfs.opfsDbDirectorySize(name), (error) => error === failure);
            sizes.mock.restore();
            file.bytes = new Uint8Array(9);
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 9n);
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('unsafe or coerced size observations preserve BigInt semantics without becoming cached offsets', async (t) => {
        const { sessionId } = await openReadSession(new Uint8Array(4));
        let rawSize = 4;
        const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize', () => rawSize);
        try {
            for (const value of [Number.MAX_SAFE_INTEGER + 1, -1, 2n, '7', NaN, 0.5, Infinity]) {
                rawSize = 4;
                assert.equal(opfs.opfsLen(sessionId, 1), 4n);
                rawSize = value;
                const sizeCalls = sizes.mock.callCount();
                for (const query of [opfs.opfsLen, opfs.opfsAppendOffset, opfs.opfsAppendOffset]) {
                    if (typeof value === 'number' && !Number.isInteger(value)) {
                        assert.throws(() => query(sessionId, 1), RangeError);
                    } else {
                        assert.equal(query(sessionId, 1), BigInt(value));
                    }
                }
                assert.equal(sizes.mock.callCount(), sizeCalls + 3);
            }
        } finally {
            opfsCloseSession(sessionId);
        }
    });

    test('unsafe write and truncate arguments keep the original call semantics and force a fresh EOF', async (t) => {
        const { sessionId, file } = await openReadSession(new Uint8Array(4));
        const sizes = t.mock.method(MemoryAccessHandle.prototype, 'getSize');
        let expectedArgument;
        t.mock.method(MemoryAccessHandle.prototype, 'write', function (bytes, { at }) {
            assert.equal(at, expectedArgument);
            this.file.bytes = new Uint8Array(9);
            return bytes.length;
        });
        t.mock.method(MemoryAccessHandle.prototype, 'truncate', function (length) {
            assert.equal(length, expectedArgument);
            this.file.bytes = new Uint8Array(9);
        });
        try {
            for (const argument of [-1n, 0.5, NaN, Infinity, BigInt(Number.MAX_SAFE_INTEGER) + 2n]) {
                expectedArgument = Number(argument);
                for (const operation of ['write', 'truncate']) {
                    file.bytes = new Uint8Array(4);
                    assert.equal(opfs.opfsLen(sessionId, 1), 4n);
                    const sizeCalls = sizes.mock.callCount();
                    if (operation === 'write') opfs.opfsWriteAt(sessionId, 1, argument, new Uint8Array(1));
                    else opfs.opfsTruncate(sessionId, 1, argument);
                    assert.equal(opfs.opfsAppendOffset(sessionId, 1), 9n);
                    assert.equal(sizes.mock.callCount(), sizeCalls + 1);
                }
            }
            expectedArgument = Number.MAX_SAFE_INTEGER;
            assert.equal(opfs.opfsLen(sessionId, 1), 9n);
            const sizeCalls = sizes.mock.callCount();
            opfs.opfsWriteAt(sessionId, 1, BigInt(Number.MAX_SAFE_INTEGER), new Uint8Array(1));
            assert.equal(opfs.opfsAppendOffset(sessionId, 1), 9n, 'the written end must also be exactly representable');
            assert.equal(sizes.mock.callCount(), sizeCalls + 1);
            const failure = new Error('numeric conversion failed');
            const invalid = {
                valueOf() {
                    file.bytes = new Uint8Array(11);
                    throw failure;
                }
            };
            for (const operation of [opfs.opfsWriteAt, opfs.opfsTruncate]) {
                file.bytes = new Uint8Array(4);
                assert.equal(opfs.opfsLen(sessionId, 1), 4n);
                assert.throws(
                    () => operation(sessionId, 1, invalid, new Uint8Array(1)),
                    (error) => error === failure
                );
                assert.equal(opfs.opfsAppendOffset(sessionId, 1), 11n);
            }
        } finally {
            opfsCloseSession(sessionId);
        }
    });
});
