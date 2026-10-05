// Injected into a dedicated Worker; this file is not imported by Node.
/* global performance, FileSystemFileHandle, FileSystemSyncAccessHandle, FileSystemDirectoryHandle, StorageManager, WebAssembly */
(() => {
    const fresh = () => ({ started: performance.now(), timeOrigin: performance.timeOrigin, metrics: {} });
    let current = fresh();
    globalThis.__gapReset = () => {
        current = fresh();
    };
    globalThis.__gapSnapshot = () => ({
        ...current,
        elapsed: performance.now() - current.started,
        resources: performance
            .getEntriesByType('resource')
            .map(({ name, startTime, duration, transferSize, encodedBodySize }) => ({
                name,
                startTime,
                duration,
                transferSize,
                encodedBodySize
            }))
    });
    const record = (name, ms, bytes = 0, requested = 0, error = false) => {
        const m = (current.metrics[name] ??= { count: 0, ms: 0, bytes: 0, requestedBytes: 0, errors: 0, sizes: {} });
        m.count++;
        m.ms += ms;
        m.bytes += bytes;
        m.requestedBytes += requested;
        m.errors += Number(error);
        if (requested) m.sizes[requested] = (m.sizes[requested] ?? 0) + 1;
    };
    globalThis.__gapRecord = record;
    const wrap = (object, key, name, argsBytes, returnedBytes) => {
        const original = object[key];
        if (typeof original !== 'function') return;
        object[key] = function (...args) {
            const start = performance.now();
            const label = typeof name === 'function' ? name.call(this, ...args) : name;
            const requested = argsBytes ? argsBytes(args) : 0;
            try {
                const out = original.apply(this, args);
                if (out && typeof out.then === 'function')
                    return out.then(
                        (value) => {
                            record(
                                label,
                                performance.now() - start,
                                returnedBytes ? returnedBytes(value) : 0,
                                requested
                            );
                            return value;
                        },
                        (error) => {
                            record(label, performance.now() - start, 0, requested, true);
                            throw error;
                        }
                    );
                record(label, performance.now() - start, returnedBytes ? returnedBytes(out) : 0, requested);
                return out;
            } catch (error) {
                record(label, performance.now() - start, 0, requested, true);
                throw error;
            }
        };
    };
    globalThis.__gapWrap = wrap;
    const names = new WeakMap();
    if (globalThis.FileSystemFileHandle) {
        const original = FileSystemFileHandle.prototype.createSyncAccessHandle;
        if (original)
            FileSystemFileHandle.prototype.createSyncAccessHandle = async function (...args) {
                const start = performance.now();
                try {
                    const handle = await original.apply(this, args);
                    names.set(handle, this.name);
                    record(`open.syncHandle:${this.name}`, performance.now() - start);
                    return handle;
                } catch (error) {
                    record(`open.syncHandle:${this.name}`, performance.now() - start, 0, 0, true);
                    throw error;
                }
            };
    }
    if (globalThis.FileSystemSyncAccessHandle) {
        for (const key of ['read', 'write', 'flush', 'getSize', 'truncate', 'close'])
            wrap(
                FileSystemSyncAccessHandle.prototype,
                key,
                function () {
                    return `storage.${names.get(this) ?? 'unknown'}.${key}`;
                },
                key === 'read' || key === 'write' ? (args) => args[0].byteLength : null,
                key === 'read' || key === 'write' ? (value) => Number(value) : null
            );
    }
    if (globalThis.FileSystemDirectoryHandle) {
        for (const key of ['getFileHandle', 'getDirectoryHandle', 'removeEntry'])
            wrap(FileSystemDirectoryHandle.prototype, key, function (name) {
                return `open.${key}:${name}`;
            });
    }
    if (globalThis.StorageManager) wrap(StorageManager.prototype, 'getDirectory', 'open.getDirectory');
    for (const key of ['instantiateStreaming', 'instantiate', 'compileStreaming', 'compile'])
        wrap(WebAssembly, key, `wasmInit.${key}`);
})();
