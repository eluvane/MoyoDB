// IO and control-file publication tests against an in-memory OPFS.
// Covers the failure modes of the original report: short writes accepted as
// success, a torn newer slot masking the valid one, and a corrupt control file
// being treated like a missing one.
import assert from 'node:assert/strict';
import { beforeEach, describe, test } from 'node:test';

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
    for (const generation of generations) {
        await dbRoot.getDirectoryHandle(generation, { create: true });
    }
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
    return { sessionId, file };
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
        assert.deepEqual(Array.from(fresh.dbRoot.children.keys()).sort(), ['gen-c-3', CONTROL_FILE_NAME]);
    });
});
