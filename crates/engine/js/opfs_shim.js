const DB_ROOT_DIR = 'stackdb';
const FILE_NAMES = ['manifest.bin', 'main.bin', 'wal.bin'];
const CONTROL_FILE_NAME = 'root-manifest.bin';
const CONTROL_SLOT_SIZE = 4096;
const CONTROL_VERSION = 1;
const CONTROL_CHECKSUM_OFFSET = 24;
const CONTROL_NAME_OFFSET = 32;
const CONTROL_MAGIC = new Uint8Array([66, 68, 66, 82, 79, 79, 84, 49]);
const OPFS_WRITE_CHUNK_SIZE = 256 * 1024;
const TEXT_ENCODER = new TextEncoder();
const TEXT_DECODER = new TextDecoder();
let rootDir = null;
let nextSessionId = 1;
const sessions = new Map();
async function getRootDir() {
    if (!navigator?.storage?.getDirectory) {
        throw new Error('navigator.storage.getDirectory is unavailable');
    }
    if (!rootDir) {
        rootDir = await navigator.storage.getDirectory();
    }
    return rootDir;
}
async function getOrCreateStackdbRoot() {
    const root = await getRootDir();
    return await root.getDirectoryHandle(DB_ROOT_DIR, { create: true });
}
async function lookupDirectoryHandle(parent, name) {
    try {
        return await parent.getDirectoryHandle(name, { create: false });
    } catch (_err) {
        return null;
    }
}
async function lookupFileHandle(parent, name) {
    try {
        return await parent.getFileHandle(name, { create: false });
    } catch (_err) {
        return null;
    }
}
function sessionPath(encodedDbName, generationName = null) {
    return generationName ? `${encodedDbName}/${generationName}` : encodedDbName;
}
function slotBytesView(bytes) {
    return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
}
function writeU64(view, offset, value) {
    view.setBigUint64(offset, BigInt(value), true);
}
function readU64(view, offset) {
    return Number(view.getBigUint64(offset, true));
}
function fnv1a32(bytes, zeroOffset = -1, zeroLength = 0) {
    let hash = 0x811c9dc5;
    const zeroEnd = zeroOffset >= 0 ? zeroOffset + zeroLength : -1;
    for (let index = 0; index < bytes.length; index += 1) {
        const value = zeroOffset >= 0 && index >= zeroOffset && index < zeroEnd ? 0 : (bytes[index] ?? 0);
        hash ^= value;
        hash = Math.imul(hash, 0x01000193) >>> 0;
    }
    return hash >>> 0;
}
function isGenerationNameValid(name) {
    return typeof name === 'string' && /^gen-[a-z0-9]+-[a-z0-9]+$/i.test(name);
}
function encodeControlSlot(generationCounter, activeGeneration) {
    if (!isGenerationNameValid(activeGeneration)) {
        throw new Error(`invalid generation name ${activeGeneration}`);
    }
    const nameBytes = TEXT_ENCODER.encode(activeGeneration);
    if (CONTROL_NAME_OFFSET + nameBytes.length > CONTROL_SLOT_SIZE) {
        throw new Error(`generation name too long: ${nameBytes.length}`);
    }
    const slot = new Uint8Array(CONTROL_SLOT_SIZE);
    const view = slotBytesView(slot);
    slot.set(CONTROL_MAGIC, 0);
    view.setUint32(8, CONTROL_VERSION, true);
    writeU64(view, 12, generationCounter);
    view.setUint16(20, nameBytes.length, true);
    view.setUint16(22, 0, true);
    view.setUint32(CONTROL_CHECKSUM_OFFSET, 0, true);
    view.setUint32(28, 0, true);
    slot.set(nameBytes, CONTROL_NAME_OFFSET);
    const checksum = fnv1a32(slot, CONTROL_CHECKSUM_OFFSET, 4);
    view.setUint32(CONTROL_CHECKSUM_OFFSET, checksum, true);
    return slot;
}
function decodeControlSlot(slotIndex, bytes) {
    if (bytes.length < CONTROL_SLOT_SIZE) {
        return null;
    }
    for (let index = 0; index < CONTROL_MAGIC.length; index += 1) {
        if (bytes[index] !== CONTROL_MAGIC[index]) {
            return null;
        }
    }
    const view = slotBytesView(bytes);
    const version = view.getUint32(8, true);
    if (version !== CONTROL_VERSION) {
        throw new Error(`unsupported control file version ${version}`);
    }
    const expectedChecksum = fnv1a32(bytes, CONTROL_CHECKSUM_OFFSET, 4);
    const storedChecksum = view.getUint32(CONTROL_CHECKSUM_OFFSET, true);
    if (expectedChecksum !== storedChecksum) {
        return null;
    }
    const nameLength = view.getUint16(20, true);
    const nameEnd = CONTROL_NAME_OFFSET + nameLength;
    if (nameEnd > CONTROL_SLOT_SIZE) {
        throw new Error(`control slot name length out of bounds: ${nameLength}`);
    }
    const activeGeneration = TEXT_DECODER.decode(bytes.subarray(CONTROL_NAME_OFFSET, nameEnd));
    if (!isGenerationNameValid(activeGeneration)) {
        throw new Error(`invalid generation in control slot: ${activeGeneration}`);
    }
    return {
        slotIndex,
        generationCounter: readU64(view, 12),
        activeGeneration
    };
}
function readControlStateFromAccessHandle(accessHandle) {
    const buffer = new Uint8Array(CONTROL_SLOT_SIZE * 2);
    const size = Number(accessHandle.getSize());
    if (size <= 0) {
        return null;
    }
    const readLength = Math.min(buffer.length, size);
    accessHandle.read(buffer.subarray(0, readLength), { at: 0 });
    const slot0 = decodeControlSlot(0, buffer.subarray(0, CONTROL_SLOT_SIZE));
    const slot1 = decodeControlSlot(1, buffer.subarray(CONTROL_SLOT_SIZE, CONTROL_SLOT_SIZE * 2));
    if (slot0 && slot1) {
        return slot0.generationCounter >= slot1.generationCounter ? slot0 : slot1;
    }
    return slot0 ?? slot1 ?? null;
}
async function readControlState(dbRoot) {
    const fileHandle = await lookupFileHandle(dbRoot, CONTROL_FILE_NAME);
    if (!fileHandle) {
        return null;
    }
    const accessHandle = await fileHandle.createSyncAccessHandle();
    try {
        return readControlStateFromAccessHandle(accessHandle);
    } finally {
        accessHandle.close();
    }
}
async function writeControlState(dbRoot, activeGeneration) {
    const fileHandle = await dbRoot.getFileHandle(CONTROL_FILE_NAME, { create: true });
    const accessHandle = await fileHandle.createSyncAccessHandle();
    try {
        const current = readControlStateFromAccessHandle(accessHandle);
        const nextSlot = current ? (current.slotIndex === 0 ? 1 : 0) : 0;
        const nextGenerationCounter = (current?.generationCounter ?? 0) + 1;
        const encoded = encodeControlSlot(nextGenerationCounter, activeGeneration);
        accessHandle.write(encoded, { at: nextSlot * CONTROL_SLOT_SIZE });
        accessHandle.flush();
    } finally {
        accessHandle.close();
    }
}
function createGenerationName() {
    const timePart = Date.now().toString(36);
    const randomBytes = new Uint8Array(4);
    crypto.getRandomValues(randomBytes);
    const randomPart = Array.from(randomBytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
    return `gen-${timePart}-${randomPart}`;
}
async function getDbRoot(encodedDbName, createIfMissing) {
    const stackdb = await getOrCreateStackdbRoot();
    let dbRoot = await lookupDirectoryHandle(stackdb, encodedDbName);
    if (!dbRoot) {
        if (!createIfMissing) {
            throw new Error(`database ${encodedDbName} does not exist`);
        }
        dbRoot = await stackdb.getDirectoryHandle(encodedDbName, { create: true });
    }
    return dbRoot;
}
async function resolveActiveDataDir(dbRoot) {
    const control = await readControlState(dbRoot);
    if (!control) {
        return {
            dirHandle: dbRoot,
            generationName: null,
            path: dbRoot.name
        };
    }
    const activeDir = await lookupDirectoryHandle(dbRoot, control.activeGeneration);
    if (!activeDir) {
        throw new Error(`active generation ${control.activeGeneration} is missing`);
    }
    return {
        dirHandle: activeDir,
        generationName: control.activeGeneration,
        path: `${dbRoot.name}/${control.activeGeneration}`
    };
}
function closeSession(sessionId) {
    const session = sessions.get(sessionId);
    if (!session) {
        return;
    }
    for (const handle of session.handles.values()) {
        try {
            handle.close();
        } catch (_err) {}
    }
    sessions.delete(sessionId);
}
function closeSessionsForDb(encodedDbName) {
    const prefix = `${encodedDbName}/`;
    for (const [sessionId, session] of Array.from(sessions.entries())) {
        if (session.path === encodedDbName || session.path.startsWith(prefix)) {
            closeSession(sessionId);
        }
    }
}
async function openSessionForDir(dbPath, dirHandle) {
    const handles = new Map();
    try {
        for (let fileKind = 0; fileKind < FILE_NAMES.length; fileKind += 1) {
            const fileName = FILE_NAMES[fileKind];
            const fileHandle = await dirHandle.getFileHandle(fileName, { create: true });
            if (typeof fileHandle.createSyncAccessHandle !== 'function') {
                throw new Error('createSyncAccessHandle is unavailable');
            }
            const accessHandle = await fileHandle.createSyncAccessHandle();
            handles.set(fileKind, accessHandle);
        }
    } catch (error) {
        for (const handle of handles.values()) {
            try {
                handle.close();
            } catch (_err) {}
        }
        throw error;
    }
    const sessionId = nextSessionId;
    nextSessionId += 1;
    sessions.set(sessionId, {
        path: dbPath,
        handles
    });
    return { sessionId };
}
function getSession(sessionId) {
    const session = sessions.get(sessionId);
    if (!session) {
        throw new Error(`no OPFS session ${sessionId}`);
    }
    return session;
}
function getAccessHandle(sessionId, fileKind) {
    const session = getSession(sessionId);
    const handle = session.handles.get(fileKind);
    if (!handle) {
        throw new Error(`no OPFS access handle for session ${sessionId} file kind ${fileKind}`);
    }
    return handle;
}
function lookupOpenFileSize(path) {
    for (const session of sessions.values()) {
        for (let fileKind = 0; fileKind < FILE_NAMES.length; fileKind += 1) {
            if (`${session.path}/${FILE_NAMES[fileKind]}` === path) {
                return Number(session.handles.get(fileKind)?.getSize() ?? 0);
            }
        }
    }
    return null;
}
async function sumDirectorySize(dirHandle, pathPrefix) {
    let total = 0;
    for await (const [name, handle] of dirHandle.entries()) {
        const entryPath = pathPrefix ? `${pathPrefix}/${name}` : name;
        if (handle.kind === 'directory') {
            total += await sumDirectorySize(handle, entryPath);
            continue;
        }
        const openSize = lookupOpenFileSize(entryPath);
        if (openSize !== null) {
            total += openSize;
            continue;
        }
        const file = await handle.getFile();
        total += file.size;
    }
    return total;
}
export async function opfsOpenActiveDb(encodedDbName, createIfMissing = true) {
    const dbRoot = await getDbRoot(encodedDbName, createIfMissing);
    const active = await resolveActiveDataDir(dbRoot);
    return await openSessionForDir(active.path, active.dirHandle);
}
export async function opfsOpenGenerationDb(encodedDbName, generationName, createIfMissing = true) {
    if (!isGenerationNameValid(generationName)) {
        throw new Error(`invalid generation name ${generationName}`);
    }
    const dbRoot = await getDbRoot(encodedDbName, createIfMissing);
    const generationDir = await dbRoot.getDirectoryHandle(generationName, { create: createIfMissing });
    return await openSessionForDir(sessionPath(encodedDbName, generationName), generationDir);
}
export function opfsReadAt(sessionId, fileKind, offset, len) {
    const handle = getAccessHandle(sessionId, fileKind);
    const buffer = new Uint8Array(len);
    handle.read(buffer, { at: Number(offset) });
    return buffer;
}
export function opfsWriteAt(sessionId, fileKind, offset, bytes) {
    const handle = getAccessHandle(sessionId, fileKind);
    const startOffset = Number(offset);
    let writtenTotal = 0;
    while (writtenTotal < bytes.length) {
        const chunk = bytes.subarray(writtenTotal, writtenTotal + OPFS_WRITE_CHUNK_SIZE);
        const rawWritten = handle.write(chunk, { at: startOffset + writtenTotal });
        const written = Number(rawWritten);
        if (!Number.isSafeInteger(written) || written < 0 || written > chunk.length) {
            throw new Error(`opfs write failed: invalid byte count ${String(rawWritten)}`);
        }
        if (written === 0 && chunk.length > 0) {
            throw new Error('opfs write failed: wrote 0 bytes');
        }
        writtenTotal += written;
    }
    return writtenTotal;
}
export function opfsFlush(sessionId, fileKind) {
    const handle = getAccessHandle(sessionId, fileKind);
    handle.flush();
}
export function opfsLen(sessionId, fileKind) {
    const handle = getAccessHandle(sessionId, fileKind);
    return BigInt(handle.getSize());
}
export function opfsTruncate(sessionId, fileKind, size) {
    const handle = getAccessHandle(sessionId, fileKind);
    handle.truncate(Number(size));
}
export function opfsCloseSession(sessionId) {
    closeSession(sessionId);
}
export async function opfsPrepareRebuildTarget(encodedDbName) {
    const dbRoot = await getDbRoot(encodedDbName, true);
    let generationName = createGenerationName();
    while (await lookupDirectoryHandle(dbRoot, generationName)) {
        generationName = createGenerationName();
    }
    await dbRoot.getDirectoryHandle(generationName, { create: true });
    return { generationName };
}
export async function opfsSwapActiveGeneration(encodedDbName, generationName) {
    if (!isGenerationNameValid(generationName)) {
        throw new Error(`invalid generation name ${generationName}`);
    }
    const dbRoot = await getDbRoot(encodedDbName, false);
    const generationDir = await lookupDirectoryHandle(dbRoot, generationName);
    if (!generationDir) {
        throw new Error(`generation ${generationName} does not exist`);
    }
    await writeControlState(dbRoot, generationName);
}
export async function opfsCleanupInactiveEntries(encodedDbName) {
    const dbRoot = await lookupDirectoryHandle(await getOrCreateStackdbRoot(), encodedDbName);
    if (!dbRoot) {
        return;
    }
    const control = await readControlState(dbRoot);
    const activeGeneration = control?.activeGeneration ?? null;
    for await (const [name, handle] of dbRoot.entries()) {
        if (name === CONTROL_FILE_NAME) {
            continue;
        }
        if (handle.kind === 'directory') {
            if (activeGeneration && name === activeGeneration) {
                continue;
            }
            if (name.startsWith('gen-')) {
                await dbRoot.removeEntry(name, { recursive: true });
            }
            continue;
        }
        if (activeGeneration && FILE_NAMES.includes(name)) {
            await dbRoot.removeEntry(name);
        }
    }
}
export async function opfsDbDirectorySize(encodedDbName) {
    const dbRoot = await lookupDirectoryHandle(await getOrCreateStackdbRoot(), encodedDbName);
    if (!dbRoot) {
        return 0;
    }
    return await sumDirectorySize(dbRoot, encodedDbName);
}
export async function opfsRemoveDb(encodedDbName) {
    closeSessionsForDb(encodedDbName);
    const stackdb = await getOrCreateStackdbRoot();
    try {
        await stackdb.removeEntry(encodedDbName, { recursive: true });
    } catch (_err) {}
}
