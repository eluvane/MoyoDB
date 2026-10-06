import fs from 'node:fs';
import { randomUUID } from 'node:crypto';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { isMainThread } from 'node:worker_threads';

const LOCK_DIRECTORY = '.moyodb.lock';
const PENDING_PREFIX = '.moyodb.pending-';
const TOKEN_PATTERN = /^[a-f\d]{8}-[a-f\d]{4}-[a-f\d]{4}-[a-f\d]{4}-[a-f\d]{12}$/i;
// NativeFileBackend rejects a larger record without deleting it.
const MAX_LEASE_RECORD_BYTES = 1024;
const MAX_LEASE_PID = 0xffffffff;
export const MISSING_DATABASE_MESSAGE = 'database missing and create_if_missing=false';
let installedStorage = null;

function namedError(name, message, cause) {
    const error = new Error(message, cause === undefined ? undefined : { cause });
    error.name = name;
    error.code = cause?.code ?? name;
    return error;
}

function storageError(error) {
    if (
        ['NotFoundError', 'TypeMismatchError', 'SecurityError', 'DatabaseBusyError', 'StorageError'].includes(
            error?.name
        )
    ) {
        return error;
    }
    if (error?.code === 'ENOENT') return namedError('NotFoundError', error.message, error);
    return namedError('StorageError', error?.message ?? String(error), error);
}

function validateDirectory(directory) {
    if (typeof directory !== 'string' || directory.length === 0 || directory.includes('\0')) {
        throw new TypeError('directory must be a nonempty filesystem path');
    }
    return resolve(directory);
}

function validateToken(token) {
    if (typeof token !== 'string' || !TOKEN_PATTERN.test(token)) throw new TypeError('invalid storage lock token');
    return token;
}

function validateName(name) {
    if (
        typeof name !== 'string' ||
        name.length === 0 ||
        name === '.' ||
        name === '..' ||
        name.toLowerCase() === LOCK_DIRECTORY ||
        name.toLowerCase().startsWith(PENDING_PREFIX) ||
        Array.from(name).some((character) => character.charCodeAt(0) < 32) ||
        /[<>:"/\\|?*]/.test(name) ||
        /[. ]$/.test(name) ||
        /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(name)
    ) {
        throw new TypeError('invalid storage entry name');
    }
    return name;
}

function entryStat(path, kind) {
    const stat = fs.lstatSync(path);
    if (stat.isSymbolicLink() || (stat.isFile() && stat.nlink !== 1)) {
        throw namedError('SecurityError', 'storage entries must not be links');
    }
    if ((kind === 'directory' && !stat.isDirectory()) || (kind === 'file' && !stat.isFile())) {
        throw namedError('TypeMismatchError', `storage entry is not a ${kind}`);
    }
    if (!stat.isDirectory() && !stat.isFile()) throw namedError('SecurityError', 'unsupported storage entry type');
    return stat;
}

function syncDirectory(path) {
    // Node cannot open directory descriptors on Windows. File flushes still use fsync.
    if (process.platform === 'win32') return;
    const fd = fs.openSync(path, fs.constants.O_RDONLY | (fs.constants.O_DIRECTORY ?? 0));
    try {
        fs.fsyncSync(fd);
    } finally {
        fs.closeSync(fd);
    }
}

function readLease(directory, token) {
    const lockDirectory = join(directory, LOCK_DIRECTORY);
    entryStat(lockDirectory, 'directory');
    const recordPath = join(lockDirectory, `${validateToken(token)}.json`);
    const stat = entryStat(recordPath, 'file');
    if (stat.size > MAX_LEASE_RECORD_BYTES) {
        throw namedError('DatabaseBusyError', 'storage lock owner cannot be verified');
    }
    const owner = JSON.parse(fs.readFileSync(recordPath, 'utf8'));
    if (owner.token !== token || !Number.isSafeInteger(owner.pid) || owner.pid <= 0 || owner.pid > MAX_LEASE_PID) {
        throw namedError('DatabaseBusyError', 'storage lock owner cannot be verified');
    }
    return { lockDirectory, recordPath, owner };
}

function removeLease(lease) {
    // Remove only this token. A newer nonempty lease must survive cleanup.
    fs.unlinkSync(lease.recordPath);
    try {
        fs.rmdirSync(lease.lockDirectory);
    } catch (error) {
        if (!['ENOENT', 'ENOTEMPTY', 'EEXIST'].includes(error?.code)) throw error;
    }
    syncDirectory(dirname(lease.lockDirectory));
}

function removeVacantLease(directory) {
    const path = join(directory, LOCK_DIRECTORY);
    try {
        entryStat(path, 'directory');
        if (fs.readdirSync(path).length !== 0) return false;
        fs.rmdirSync(path);
        syncDirectory(directory);
        return true;
    } catch (error) {
        if (['ENOENT', 'ENOTEMPTY', 'EEXIST'].includes(error?.code)) return false;
        throw error;
    }
}

function publishLease(directory, pending) {
    const lockDirectory = join(directory, LOCK_DIRECTORY);
    for (let attempt = 0; attempt < 3; attempt += 1) {
        let published = false;
        try {
            fs.renameSync(pending, lockDirectory);
            published = true;
        } catch (error) {
            try {
                fs.lstatSync(lockDirectory);
            } catch {
                throw storageError(error);
            }
        }
        if (published) {
            syncDirectory(directory);
            return;
        }
        let lease;
        try {
            entryStat(lockDirectory, 'directory');
            const records = fs.readdirSync(lockDirectory);
            if (records.length === 0) {
                removeVacantLease(directory);
                continue;
            }
            if (records.length !== 1 || !records[0].endsWith('.json')) throw new Error('incomplete lock record');
            lease = readLease(directory, records[0].slice(0, -5));
            try {
                process.kill(lease.owner.pid, 0);
            } catch (failure) {
                if (failure?.code === 'ESRCH') {
                    removeLease(lease);
                    continue;
                }
                throw failure;
            }
        } catch (failure) {
            throw namedError('DatabaseBusyError', 'storage directory is locked; its owner cannot be released', failure);
        }
        throw namedError('DatabaseBusyError', `storage directory is owned by process ${lease.owner.pid}`);
    }
    throw namedError('DatabaseBusyError', 'storage directory changed while acquiring its lock');
}

function removePendingLease(directory, token) {
    const pending = join(directory, `${PENDING_PREFIX}${process.pid}-${token}`);
    try {
        entryStat(pending, 'directory');
    } catch (error) {
        if (error?.code === 'ENOENT') return false;
        throw error;
    }
    const entries = fs.readdirSync(pending);
    if (entries.length === 1 && entries[0] === `${token}.json`) {
        const recordPath = join(pending, entries[0]);
        entryStat(recordPath, 'file');
        fs.unlinkSync(recordPath);
    } else if (entries.length !== 0) {
        throw namedError('SecurityError', 'pending storage lock contains unknown entries');
    }
    fs.rmdirSync(pending);
    return true;
}

function acquireLease(directory, token) {
    const pending = join(directory, `${PENDING_PREFIX}${process.pid}-${token}`);
    const recordPath = join(pending, `${token}.json`);
    let fd;
    let created = false;
    try {
        fs.mkdirSync(pending, { mode: 0o700 });
        created = true;
        fd = fs.openSync(recordPath, 'wx', 0o600);
        fs.writeFileSync(fd, JSON.stringify({ pid: process.pid, token }));
        fs.fsyncSync(fd);
        fs.closeSync(fd);
        fd = undefined;
        syncDirectory(pending);
        // Publish the complete owner record. Termination before rename cannot block a later owner.
        publishLease(directory, pending);
    } catch (error) {
        const primary = storageError(error);
        if (fd !== undefined) {
            try {
                fs.closeSync(fd);
            } catch (cleanupError) {
                primary.cleanupErrors = [cleanupError];
            }
        }
        if (created) {
            try {
                releaseNodeStorageLease(directory, token);
            } catch (cleanupError) {
                primary.cleanupErrors = [...(primary.cleanupErrors ?? []), cleanupError];
            }
        }
        throw primary;
    }
}

export function releaseNodeStorageLease(directory, lockToken) {
    const path = validateDirectory(directory);
    validateToken(lockToken);
    let lease;
    let canonical;
    try {
        canonical = fs.realpathSync(path);
        lease = readLease(canonical, lockToken);
    } catch (error) {
        if (error?.code === 'ENOTDIR') return false;
        if (error?.code === 'ENOENT') {
            if (!canonical) return false;
            const removedPending = removePendingLease(canonical, lockToken);
            return removeVacantLease(canonical) || removedPending;
        }
        throw storageError(error);
    }
    if (lease.owner.pid !== process.pid) return false;
    try {
        removeLease(lease);
        removePendingLease(canonical, lockToken);
        return true;
    } catch (error) {
        if (error?.code === 'ENOENT') return false;
        throw storageError(error);
    }
}

function validPosition(value, label) {
    if (!Number.isSafeInteger(value) || value < 0) throw new RangeError(`${label} must be a nonnegative safe integer`);
    return value;
}

class AccessHandle {
    #storage;
    #path;
    #fd;

    constructor(storage, path, fd) {
        this.#storage = storage;
        this.#path = path;
        this.#fd = fd;
    }

    #live() {
        this.#storage.live();
        if (this.#fd === null) throw namedError('InvalidStateError', 'storage access handle is closed');
        return this.#fd;
    }

    read(buffer, { at = 0 } = {}) {
        return fs.readSync(this.#live(), buffer, 0, buffer.byteLength, validPosition(at, 'read offset'));
    }

    write(buffer, { at = 0 } = {}) {
        validPosition(at, 'write offset');
        validPosition(at + buffer.byteLength, 'write end');
        return fs.writeSync(this.#live(), buffer, 0, buffer.byteLength, at);
    }

    getSize() {
        return validPosition(fs.fstatSync(this.#live()).size, 'file size');
    }

    truncate(size) {
        fs.ftruncateSync(this.#live(), validPosition(size, 'file size'));
    }

    flush() {
        fs.fsyncSync(this.#live());
    }

    close() {
        if (this.#fd === null) return;
        const fd = this.#fd;
        this.#fd = null;
        try {
            fs.closeSync(fd);
        } finally {
            this.#storage.handles.delete(this.#path);
        }
    }
}

class FileHandle {
    kind = 'file';

    constructor(storage, path) {
        this.storage = storage;
        this.path = path;
        this.name = basename(path);
    }

    async createSyncAccessHandle() {
        this.storage.check(this.path, 'file');
        if (this.storage.handles.has(this.path)) {
            throw namedError('NoModificationAllowedError', 'storage file already has an access handle');
        }
        let fd;
        try {
            fd = fs.openSync(this.path, fs.constants.O_RDWR | (fs.constants.O_NOFOLLOW ?? 0));
            const stat = fs.fstatSync(fd);
            const expected = this.storage.check(this.path, 'file');
            if (!stat.isFile() || stat.nlink !== 1 || stat.dev !== expected.dev || stat.ino !== expected.ino) {
                throw namedError('SecurityError', 'storage file changed while opening');
            }
            const handle = new AccessHandle(this.storage, this.path, fd);
            this.storage.handles.set(this.path, handle);
            return handle;
        } catch (error) {
            const primary = storageError(error);
            if (fd !== undefined) {
                try {
                    fs.closeSync(fd);
                } catch (cleanupError) {
                    primary.cleanupErrors = [cleanupError];
                }
            }
            throw primary;
        }
    }

    async getFile() {
        return { size: this.storage.check(this.path, 'file').size };
    }
}

class DirectoryHandle {
    kind = 'directory';

    constructor(storage, path) {
        this.storage = storage;
        this.path = path;
        this.name = basename(path);
    }

    async getDirectoryHandle(name, { create = false } = {}) {
        this.storage.check(this.path, 'directory');
        const path = join(this.path, validateName(name));
        try {
            if (create) {
                try {
                    fs.mkdirSync(path, { mode: 0o700 });
                    syncDirectory(this.path);
                } catch (error) {
                    if (error?.code !== 'EEXIST') throw error;
                }
            }
            this.storage.check(path, 'directory');
            return new DirectoryHandle(this.storage, path);
        } catch (error) {
            if (error?.name === 'TypeMismatchError' || error?.name === 'SecurityError') throw error;
            throw storageError(error);
        }
    }

    async getFileHandle(name, { create = false } = {}) {
        this.storage.check(this.path, 'directory');
        const path = join(this.path, validateName(name));
        try {
            if (create) {
                let fd;
                try {
                    fd = fs.openSync(path, 'wx', 0o600);
                    fs.fsyncSync(fd);
                } catch (error) {
                    if (error?.code !== 'EEXIST') throw error;
                } finally {
                    if (fd !== undefined) fs.closeSync(fd);
                }
                if (fd !== undefined) syncDirectory(this.path);
            }
            this.storage.check(path, 'file');
            return new FileHandle(this.storage, path);
        } catch (error) {
            if (error?.name === 'TypeMismatchError' || error?.name === 'SecurityError') throw error;
            throw storageError(error);
        }
    }

    async *entries() {
        this.storage.check(this.path, 'directory');
        for (const name of fs.readdirSync(this.path).sort()) {
            if (this.path === this.storage.directory && (name === LOCK_DIRECTORY || name.startsWith(PENDING_PREFIX)))
                continue;
            validateName(name);
            const path = join(this.path, name);
            const stat = this.storage.check(path);
            yield [
                name,
                stat.isDirectory() ? new DirectoryHandle(this.storage, path) : new FileHandle(this.storage, path)
            ];
        }
    }

    async removeEntry(name, { recursive = false } = {}) {
        this.storage.check(this.path, 'directory');
        const path = join(this.path, validateName(name));
        const stat = this.storage.check(path);
        for (const openPath of this.storage.handles.keys()) {
            if (openPath === path || openPath.startsWith(`${path}${sep}`)) {
                throw namedError('NoModificationAllowedError', 'storage entry has an open access handle');
            }
        }
        try {
            if (stat.isDirectory()) {
                if (recursive) this.storage.removeTree(path);
                else fs.rmdirSync(path);
            } else {
                fs.unlinkSync(path);
            }
            syncDirectory(this.path);
        } catch (error) {
            if (error?.name === 'SecurityError') throw error;
            throw storageError(error);
        }
    }
}

class Storage {
    handles = new Map();
    closed = false;

    constructor(directory) {
        this.directory = directory;
        this.identity = entryStat(directory, 'directory');
    }

    live() {
        if (this.closed) throw namedError('InvalidStateError', 'Node storage is closed');
    }

    check(path, kind) {
        this.live();
        const parts = relative(this.directory, path).split(sep).filter(Boolean);
        if (parts.some((name) => name === '..')) throw namedError('SecurityError', 'storage path leaves its directory');
        let current = this.directory;
        let stat;
        try {
            stat = entryStat(current, 'directory');
            if (stat.dev !== this.identity.dev || stat.ino !== this.identity.ino) {
                throw namedError('SecurityError', 'storage directory changed');
            }
            for (let index = 0; index < parts.length; index += 1) {
                current = join(current, parts[index]);
                stat = entryStat(current, index + 1 === parts.length ? kind : 'directory');
            }
            return stat;
        } catch (error) {
            if (error?.name === 'TypeMismatchError' || error?.name === 'SecurityError') throw error;
            throw storageError(error);
        }
    }

    removeTree(path) {
        for (const name of fs.readdirSync(path)) {
            const child = join(path, validateName(name));
            const stat = this.check(child);
            if (stat.isDirectory()) this.removeTree(child);
            else fs.unlinkSync(child);
        }
        fs.rmdirSync(path);
    }
}

class MountedDirectoryHandle {
    kind = 'directory';

    constructor(storage, name, childName, child, remove) {
        this.storage = storage;
        this.name = name;
        this.childName = childName;
        this.child = child;
        this.remove = remove;
    }

    async getDirectoryHandle(name, { create = false } = {}) {
        this.storage.live();
        if (validateName(name) !== this.childName)
            throw namedError('NotFoundError', 'directory is outside the database mount');
        return this.child(create);
    }

    async getFileHandle(name) {
        this.storage.live();
        validateName(name);
        if (name === this.childName) throw namedError('TypeMismatchError', 'mounted entry is a directory');
        throw namedError('NotFoundError', 'file is outside the database mount');
    }

    async *entries() {
        this.storage.live();
        let child;
        try {
            child = this.child(false);
        } catch (error) {
            if (error?.name === 'NotFoundError') return;
            throw error;
        }
        yield [this.childName, child];
    }

    async removeEntry(name, { recursive = false } = {}) {
        this.storage.live();
        if (validateName(name) !== this.childName)
            throw namedError('NotFoundError', 'directory is outside the database mount');
        if (!this.remove) throw namedError('NoModificationAllowedError', 'the storage mount cannot be removed');
        await this.remove(recursive);
    }
}

function mountedRoot(storage, encodedDbName) {
    const database = new DirectoryHandle(storage, storage.directory);
    database.name = encodedDbName;
    let exists = true;
    const stackdb = new MountedDirectoryHandle(
        storage,
        'stackdb',
        encodedDbName,
        (create) => {
            if (!exists && !create) throw namedError('NotFoundError', 'database directory does not exist');
            exists = true;
            return database;
        },
        async (recursive) => {
            if (!exists) throw namedError('NotFoundError', 'database directory does not exist');
            const names = [];
            for await (const [name] of database.entries()) names.push(name);
            if (!recursive && names.length !== 0)
                throw namedError('InvalidModificationError', 'database directory is not empty');
            for (const name of names) await database.removeEntry(name, { recursive });
            // Keep the physical database directory and its lease until the worker stops.
            exists = false;
        }
    );
    return new MountedDirectoryHandle(storage, '', 'stackdb', () => stackdb);
}

export function ensureStorageRoot(directory, createIfMissing = true) {
    const path = validateDirectory(directory);
    if (!createIfMissing) {
        try {
            entryStat(path, 'directory');
        } catch (error) {
            if (error?.code === 'ENOENT') throw namedError('StorageError', MISSING_DATABASE_MESSAGE);
            if (error?.name === 'TypeMismatchError' || error?.name === 'SecurityError') throw error;
            throw storageError(error);
        }
        return path;
    }
    fs.mkdirSync(path, { recursive: true });
    return path;
}

export async function installNodeStorage(
    directory,
    { lockToken = randomUUID(), encodedDbName, createIfMissing = true } = {}
) {
    if (isMainThread) throw new Error('Node storage must be installed in a dedicated worker');
    if (installedStorage) throw new Error('Node storage is already installed in this worker');
    const path = validateDirectory(directory);
    validateToken(lockToken);
    if (
        encodedDbName !== undefined &&
        (typeof encodedDbName !== 'string' || !/^(?:[a-f\d]{2})+$/.test(encodedDbName))
    ) {
        throw new TypeError('encodedDbName must be the lowercase hexadecimal UTF-8 database name');
    }
    if (createIfMissing === false) {
        try {
            entryStat(path, 'directory');
        } catch (error) {
            if (error?.code === 'ENOENT') throw namedError('StorageError', MISSING_DATABASE_MESSAGE);
            if (error?.name === 'TypeMismatchError' || error?.name === 'SecurityError') throw error;
            throw storageError(error);
        }
    }
    let canonical;
    try {
        fs.mkdirSync(path, { recursive: true, mode: 0o700 });
        canonical = fs.realpathSync(path);
        entryStat(canonical, 'directory');
    } catch (error) {
        throw storageError(error);
    }
    acquireLease(canonical, lockToken);
    const previousNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
    let storage;
    try {
        storage = new Storage(canonical);
    } catch (error) {
        releaseNodeStorageLease(canonical, lockToken);
        throw storageError(error);
    }
    let closed = false;
    const close = () => {
        if (closed) return;
        closed = true;
        let failure;
        for (const handle of storage.handles.values()) {
            try {
                handle.close();
            } catch (error) {
                failure ??= error;
            }
        }
        storage.closed = true;
        try {
            releaseNodeStorageLease(canonical, lockToken);
        } catch (error) {
            failure ??= error;
        }
        try {
            if (previousNavigator) Object.defineProperty(globalThis, 'navigator', previousNavigator);
            else delete globalThis.navigator;
        } catch (error) {
            failure ??= error;
        }
        installedStorage = null;
        if (failure) throw failure;
    };
    try {
        const root =
            encodedDbName === undefined ? new DirectoryHandle(storage, canonical) : mountedRoot(storage, encodedDbName);
        Object.defineProperty(globalThis, 'navigator', {
            configurable: true,
            value: {
                storage: {
                    getDirectory: async () => {
                        storage.live();
                        return root;
                    }
                }
            }
        });
        installedStorage = storage;
        return { directory: canonical, close };
    } catch (error) {
        close();
        throw error;
    }
}
