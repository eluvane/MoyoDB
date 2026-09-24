// Control-file publication tests for opfs_shim.js against an in-memory OPFS.
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
    /** Replaces the default write; receives (src, at, file) and returns the byte count. */
    writeHook = null;
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
        const count = Math.max(0, Math.min(dst.length, this.file.bytes.length - at));
        dst.set(this.file.bytes.subarray(at, at + count));
        return count;
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

const {
    decodeControlSlot,
    encodeControlSlot,
    opfsCleanupInactiveEntries,
    opfsReadActiveGeneration,
    opfsSwapActiveGeneration,
    readControlStateFromAccessHandle,
    writeAll
} = await import('./opfs_shim.js');

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
