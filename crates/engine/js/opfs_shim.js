const DB_ROOT_DIR = 'stackdb';
const FILE_NAMES = ['manifest.bin', 'main.bin', 'wal.bin'];
const CONTROL_FILE_NAME = 'root-manifest.bin';
const CONTROL_SLOT_SIZE = 4096;
const CONTROL_VERSION = 1;
const CONTROL_CHECKSUM_OFFSET = 24;
const CONTROL_NAME_OFFSET = 32;
const CONTROL_MAGIC = new Uint8Array([66, 68, 66, 82, 79, 79, 84, 49]);
// Chromium rejects a SyncAccessHandle.write larger than a signed 32-bit count.
// This is an API limit, not a batching target; the engine already owns the input.
const OPFS_MAX_WRITE_SIZE = 0x7fffffff;
const TEXT_ENCODER = new TextEncoder();
const TEXT_DECODER = new TextDecoder();
let rootDir = null;
let nextSessionId = 1;
const sessions = new Map();
// Session data handles are private and exclusively owned. Rust backend aliases
// share a handle, so successful writes/truncates keep one append offset coherent.
// Authoritative length reads still reach OPFS for validation and statistics.
const appendOffsets = new WeakMap();
function namedError(name, message) {
    const error = new Error(message);
    error.name = name;
    error.code = name;
    return error;
}
function corruptionError(message) {
    return namedError('CorruptionError', message);
}
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
function isNotFoundError(error) {
    return error?.name === 'NotFoundError' || error?.name === 'TypeMismatchError';
}
async function lookupDirectoryHandle(parent, name) {
    try {
        return await parent.getDirectoryHandle(name, { create: false });
    } catch (err) {
        if (isNotFoundError(err)) {
            return null;
        }
        throw err;
    }
}
async function lookupFileHandle(parent, name) {
    try {
        return await parent.getFileHandle(name, { create: false });
    } catch (err) {
        if (isNotFoundError(err)) {
            return null;
        }
        throw err;
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
// Every byte must reach the file or the call fails: a short write that returns
// normally would let callers publish state that is not on disk.
export function writeAll(handle, bytes, at) {
    let writtenTotal = 0;
    while (writtenTotal < bytes.length) {
        // Preserve the engine's batch instead of fragmenting a WAL append.
        // Only the browser's size limit or a short write needs a suffix view.
        const end = Math.min(bytes.length, writtenTotal + OPFS_MAX_WRITE_SIZE);
        const chunk = writtenTotal === 0 && end === bytes.length ? bytes : bytes.subarray(writtenTotal, end);
        const rawWritten = handle.write(chunk, { at: at + writtenTotal });
        const written = Number(rawWritten);
        if (!Number.isSafeInteger(written) || written < 0 || written > chunk.length) {
            throw namedError('StorageError', `opfs write failed: invalid byte count ${String(rawWritten)}`);
        }
        if (written === 0 && chunk.length > 0) {
            throw namedError('StorageError', 'opfs write failed: wrote 0 bytes');
        }
        writtenTotal += written;
    }
    return writtenTotal;
}
// Fills `buffer` from `at`, stopping only at end of file. Bytes past EOF stay zero.
export function readAll(handle, buffer, at) {
    let readTotal = 0;
    while (readTotal < buffer.length) {
        const rawRead = handle.read(buffer.subarray(readTotal), { at: at + readTotal });
        const read = Number(rawRead);
        if (!Number.isSafeInteger(read) || read < 0 || read > buffer.length - readTotal) {
            throw namedError('StorageError', `opfs read failed: invalid byte count ${String(rawRead)}`);
        }
        if (read === 0) {
            break;
        }
        readTotal += read;
    }
    return readTotal;
}
export function encodeControlSlot(generationCounter, activeGeneration) {
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
// Returns null for any slot whose bytes do not checksum. Fields are interpreted
// only after the checksum passes, so a torn slot never masks its valid sibling.
export function decodeControlSlot(slotIndex, bytes) {
    if (bytes.length < CONTROL_SLOT_SIZE) {
        return null;
    }
    for (let index = 0; index < CONTROL_MAGIC.length; index += 1) {
        if (bytes[index] !== CONTROL_MAGIC[index]) {
            return null;
        }
    }
    const view = slotBytesView(bytes);
    const expectedChecksum = fnv1a32(bytes.subarray(0, CONTROL_SLOT_SIZE), CONTROL_CHECKSUM_OFFSET, 4);
    const storedChecksum = view.getUint32(CONTROL_CHECKSUM_OFFSET, true);
    if (expectedChecksum !== storedChecksum) {
        return null;
    }
    const version = view.getUint32(8, true);
    if (version !== CONTROL_VERSION) {
        throw corruptionError(`unsupported control file version ${version}`);
    }
    const nameLength = view.getUint16(20, true);
    const nameEnd = CONTROL_NAME_OFFSET + nameLength;
    if (nameEnd > CONTROL_SLOT_SIZE) {
        throw corruptionError(`control slot name length out of bounds: ${nameLength}`);
    }
    const activeGeneration = TEXT_DECODER.decode(bytes.subarray(CONTROL_NAME_OFFSET, nameEnd));
    if (!isGenerationNameValid(activeGeneration)) {
        throw corruptionError(`invalid generation in control slot: ${activeGeneration}`);
    }
    return {
        slotIndex,
        generationCounter: readU64(view, 12),
        activeGeneration
    };
}
// `absent`  — the file is empty: no generation has ever been published.
// `invalid` — the file has bytes but no slot checksums.
// `valid`   — `control` is the newest checksummed slot.
export function readControlStateFromAccessHandle(accessHandle) {
    const size = Number(accessHandle.getSize());
    if (!Number.isSafeInteger(size) || size < 0) {
        throw namedError('StorageError', `invalid control file size ${String(size)}`);
    }
    if (size === 0) {
        return { status: 'absent', control: null };
    }
    const buffer = new Uint8Array(CONTROL_SLOT_SIZE * 2);
    readAll(accessHandle, buffer.subarray(0, Math.min(buffer.length, size)), 0);
    const slot0 = decodeControlSlot(0, buffer.subarray(0, CONTROL_SLOT_SIZE));
    const slot1 = decodeControlSlot(1, buffer.subarray(CONTROL_SLOT_SIZE, CONTROL_SLOT_SIZE * 2));
    if (slot0 && slot1) {
        return { status: 'valid', control: slot0.generationCounter >= slot1.generationCounter ? slot0 : slot1 };
    }
    const control = slot0 ?? slot1;
    return control ? { status: 'valid', control } : { status: 'invalid', control: null };
}
async function readControlFile(dbRoot) {
    const fileHandle = await lookupFileHandle(dbRoot, CONTROL_FILE_NAME);
    if (!fileHandle) {
        return { status: 'absent', control: null };
    }
    const accessHandle = await fileHandle.createSyncAccessHandle();
    try {
        return readControlStateFromAccessHandle(accessHandle);
    } finally {
        accessHandle.close();
    }
}
async function hasLegacyDataFiles(dbRoot, fileKind = 0) {
    if (fileKind === FILE_NAMES.length) {
        return false;
    }
    if (await lookupFileHandle(dbRoot, FILE_NAMES[fileKind])) {
        return true;
    }
    return await hasLegacyDataFiles(dbRoot, fileKind + 1);
}
// Resolves which directory holds the live database, failing closed on damage.
//
// Legacy root files are removed before the first write into a published
// generation (see opfsCleanupInactiveEntries and opfsOpenActiveDb). So an
// unreadable control file next to legacy files can only be a torn first
// publication, and legacy is still authoritative. Without legacy files it is
// real corruption of the pointer to live data.
async function resolveActiveGeneration(dbRoot) {
    const state = await readControlFile(dbRoot);
    if (state.status === 'valid') {
        return state.control.activeGeneration;
    }
    if (state.status === 'absent') {
        return null;
    }
    if (await hasLegacyDataFiles(dbRoot)) {
        return null;
    }
    throw corruptionError(`control file ${CONTROL_FILE_NAME} is corrupt: no slot has a valid checksum`);
}
async function writeControlState(dbRoot, activeGeneration, expectedCurrentGeneration) {
    const fileHandle = await dbRoot.getFileHandle(CONTROL_FILE_NAME, { create: true });
    const accessHandle = await fileHandle.createSyncAccessHandle();
    try {
        const current = readControlStateFromAccessHandle(accessHandle);
        if (current.status === 'invalid' && !(await hasLegacyDataFiles(dbRoot))) {
            throw corruptionError('refusing to publish a generation over a corrupt control file');
        }
        const currentGeneration = current.control?.activeGeneration ?? null;
        if (expectedCurrentGeneration !== undefined && currentGeneration !== expectedCurrentGeneration) {
            throw namedError(
                'DatabaseBusyError',
                `active generation changed underneath compaction: expected ${String(expectedCurrentGeneration)}, found ${String(currentGeneration)}`
            );
        }
        const nextSlot = current.control ? (current.control.slotIndex === 0 ? 1 : 0) : 0;
        const nextGenerationCounter = (current.control?.generationCounter ?? 0) + 1;
        const encoded = encodeControlSlot(nextGenerationCounter, activeGeneration);
        writeAll(accessHandle, encoded, nextSlot * CONTROL_SLOT_SIZE);
        accessHandle.flush();
        const published = readControlStateFromAccessHandle(accessHandle);
        if (
            published.status !== 'valid' ||
            published.control.activeGeneration !== activeGeneration ||
            published.control.generationCounter !== nextGenerationCounter
        ) {
            throw namedError('StorageError', `control file does not select ${activeGeneration} after publication`);
        }
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
async function createGenerationDirectory(dbRoot) {
    const generationName = createGenerationName();
    if (await lookupDirectoryHandle(dbRoot, generationName)) {
        return await createGenerationDirectory(dbRoot);
    }
    await dbRoot.getDirectoryHandle(generationName, { create: true });
    return generationName;
}
async function getDbRoot(encodedDbName, createIfMissing) {
    const stackdb = await getOrCreateStackdbRoot();
    if (createIfMissing) {
        // This already returns an existing directory. A failed lookup first
        // adds a storage round trip to every newly created database.
        return await stackdb.getDirectoryHandle(encodedDbName, { create: true });
    }
    const dbRoot = await lookupDirectoryHandle(stackdb, encodedDbName);
    if (!dbRoot) {
        throw new Error(`database ${encodedDbName} does not exist`);
    }
    return dbRoot;
}
async function removeLegacyDataFiles(dbRoot) {
    await FILE_NAMES.reduce(async (previous, fileName) => {
        await previous;
        try {
            await dbRoot.removeEntry(fileName);
        } catch (err) {
            if (!isNotFoundError(err)) {
                throw err;
            }
        }
    }, Promise.resolve());
}
async function resolveActiveDataDir(dbRoot) {
    const activeGeneration = await resolveActiveGeneration(dbRoot);
    if (!activeGeneration) {
        return {
            dirHandle: dbRoot,
            generationName: null,
            path: dbRoot.name
        };
    }
    const activeDir = await lookupDirectoryHandle(dbRoot, activeGeneration);
    if (!activeDir) {
        throw corruptionError(`active generation ${activeGeneration} is missing`);
    }
    // A crash after publication but before legacy cleanup leaves stale root
    // files. They must be gone before this generation accepts writes.
    await removeLegacyDataFiles(dbRoot);
    return {
        dirHandle: activeDir,
        generationName: activeGeneration,
        path: `${dbRoot.name}/${activeGeneration}`
    };
}
function closeSession(sessionId) {
    const session = sessions.get(sessionId);
    if (!session) {
        return;
    }
    for (const handle of session.handles.values()) {
        appendOffsets.delete(handle);
        try {
            handle.close();
        } catch {}
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
async function openSessionForDir(dbPath, dirHandle, generationName) {
    // Generation resolution and legacy cleanup have already finished. Only
    // independent file acquisitions overlap; no session is visible yet.
    const opened = await Promise.allSettled(
        FILE_NAMES.map(async (fileName) => {
            const fileHandle = await dirHandle.getFileHandle(fileName, { create: true });
            if (typeof fileHandle.createSyncAccessHandle !== 'function') {
                throw new Error('createSyncAccessHandle is unavailable');
            }
            return await fileHandle.createSyncAccessHandle();
        })
    );
    const handles = new Map();
    for (let fileKind = 0; fileKind < opened.length; fileKind += 1) {
        const result = opened[fileKind];
        if (result.status === 'fulfilled') {
            handles.set(fileKind, result.value);
        }
    }
    const failed = opened.find((result) => result.status === 'rejected');
    if (failed) {
        // Drain every acquisition before failing, including late successes.
        // Promise.all would let those handles escape cleanup after rejection.
        for (const handle of handles.values()) {
            try {
                handle.close();
            } catch {}
        }
        // Preserve file-order error precedence, not completion-order races.
        throw failed.reason;
    }
    const sessionId = nextSessionId;
    nextSessionId += 1;
    sessions.set(sessionId, {
        path: dbPath,
        handles
    });
    return { sessionId, generationName };
}
function getSession(sessionId) {
    const session = sessions.get(sessionId);
    if (!session) {
        throw namedError('StorageError', `no OPFS session ${sessionId}`);
    }
    return session;
}
function getAccessHandle(sessionId, fileKind) {
    const session = getSession(sessionId);
    const handle = session.handles.get(fileKind);
    if (!handle) {
        throw namedError('StorageError', `no OPFS access handle for session ${sessionId} file kind ${fileKind}`);
    }
    return handle;
}
function rememberFileSize(handle, size) {
    if (typeof size === 'number' && Number.isSafeInteger(size) && size >= 0) {
        appendOffsets.set(handle, size);
    } else {
        appendOffsets.delete(handle);
    }
}
function readFileSize(handle) {
    try {
        const size = handle.getSize();
        rememberFileSize(handle, size);
        return size;
    } catch (error) {
        appendOffsets.delete(handle);
        throw error;
    }
}
function lookupOpenFileSize(path) {
    for (const session of sessions.values()) {
        for (let fileKind = 0; fileKind < FILE_NAMES.length; fileKind += 1) {
            if (`${session.path}/${FILE_NAMES[fileKind]}` === path) {
                const handle = session.handles.get(fileKind);
                return Number((handle ? readFileSize(handle) : 0) ?? 0);
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
    return await openSessionForDir(active.path, active.dirHandle, active.generationName);
}
export async function opfsOpenGenerationDb(encodedDbName, generationName, createIfMissing = true) {
    if (!isGenerationNameValid(generationName)) {
        throw new Error(`invalid generation name ${generationName}`);
    }
    const dbRoot = await getDbRoot(encodedDbName, createIfMissing);
    const generationDir = await dbRoot.getDirectoryHandle(generationName, { create: createIfMissing });
    return await openSessionForDir(sessionPath(encodedDbName, generationName), generationDir, generationName);
}
export async function opfsReadActiveGeneration(encodedDbName) {
    const dbRoot = await getDbRoot(encodedDbName, false);
    return await resolveActiveGeneration(dbRoot);
}
// Rust supplies its owned WASM buffer as a view valid only during this call.
// Keep the requested length contract even for a reused destination past EOF.
export function opfsReadAtInto(sessionId, fileKind, offset, buffer) {
    const handle = getAccessHandle(sessionId, fileKind);
    const read = readAll(handle, buffer, Number(offset));
    buffer.fill(0, read);
    return read;
}
export function opfsReadAt(sessionId, fileKind, offset, len) {
    const handle = getAccessHandle(sessionId, fileKind);
    const buffer = new Uint8Array(len);
    readAll(handle, buffer, Number(offset));
    return buffer;
}
export function opfsWriteAt(sessionId, fileKind, offset, bytes) {
    const handle = getAccessHandle(sessionId, fileKind);
    try {
        const at = Number(offset);
        const written = writeAll(handle, bytes, at);
        // An empty write never calls OPFS and cannot extend the file. An
        // unobserved EOF cannot be inferred from a successful overwrite.
        if (written > 0) {
            const previous = appendOffsets.get(handle);
            const end = at + written;
            if (previous !== undefined && Number.isSafeInteger(at) && at >= 0 && Number.isSafeInteger(end)) {
                appendOffsets.set(handle, Math.max(previous, end));
            } else {
                appendOffsets.delete(handle);
            }
        }
        return written;
    } catch (error) {
        // A failed call can still have written a prefix. Recovery must observe
        // the actual file, including when the browser throws a non-Error value.
        appendOffsets.delete(handle);
        throw error;
    }
}
export function opfsFlush(sessionId, fileKind) {
    const handle = getAccessHandle(sessionId, fileKind);
    try {
        handle.flush();
    } catch (error) {
        appendOffsets.delete(handle);
        throw error;
    }
}
export function opfsLen(sessionId, fileKind) {
    return BigInt(readFileSize(getAccessHandle(sessionId, fileKind)));
}
export function opfsAppendOffset(sessionId, fileKind) {
    const handle = getAccessHandle(sessionId, fileKind);
    const size = appendOffsets.get(handle);
    return size === undefined ? opfsLen(sessionId, fileKind) : BigInt(size);
}
export function opfsTruncate(sessionId, fileKind, size) {
    const handle = getAccessHandle(sessionId, fileKind);
    try {
        const length = Number(size);
        handle.truncate(length);
        rememberFileSize(handle, length);
    } catch (error) {
        appendOffsets.delete(handle);
        throw error;
    }
}
export function opfsCloseSession(sessionId) {
    closeSession(sessionId);
}
export async function opfsPrepareRebuildTarget(encodedDbName) {
    const dbRoot = await getDbRoot(encodedDbName, true);
    const generationName = await createGenerationDirectory(dbRoot);
    return { generationName };
}
// Publishes `generationName` as the live database. Resolves only after the
// control file has been flushed and read back selecting the new generation;
// the caller may switch its live engine only after that.
export async function opfsSwapActiveGeneration(encodedDbName, generationName, expectedCurrentGeneration) {
    if (!isGenerationNameValid(generationName)) {
        throw new Error(`invalid generation name ${generationName}`);
    }
    const dbRoot = await getDbRoot(encodedDbName, false);
    const generationDir = await lookupDirectoryHandle(dbRoot, generationName);
    if (!generationDir) {
        throw new Error(`generation ${generationName} does not exist`);
    }
    await writeControlState(dbRoot, generationName, expectedCurrentGeneration ?? null);
}
// Legacy root files are removed strictly (see resolveActiveGeneration); stale
// generation directories are best effort because nothing can select them.
export async function opfsCleanupInactiveEntries(encodedDbName) {
    const dbRoot = await lookupDirectoryHandle(await getOrCreateStackdbRoot(), encodedDbName);
    if (!dbRoot) {
        return;
    }
    const activeGeneration = await resolveActiveGeneration(dbRoot);
    if (activeGeneration) {
        await removeLegacyDataFiles(dbRoot);
    }
    const staleDirectories = [];
    for await (const [name, handle] of dbRoot.entries()) {
        if (handle.kind === 'directory' && name.startsWith('gen-') && name !== activeGeneration) {
            staleDirectories.push(name);
        }
    }
    await staleDirectories.reduce(async (previous, name) => {
        await previous;
        try {
            await dbRoot.removeEntry(name, { recursive: true });
        } catch {}
    }, Promise.resolve());
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
    } catch (err) {
        if (!isNotFoundError(err)) {
            throw err;
        }
    }
}
