import { isRecord, withTimeout } from './internal';
import { WorkerProtocolClient } from './worker-client';
import { deserializeWorkerError, serializeWorkerError, workerProtocolError } from './worker-protocol';
import {
    SHARED_WORKER_CONTROL,
    SHARED_WORKER_CONTROL_RESULT,
    SHARED_WORKER_FATAL,
    SHARED_WORKER_HEARTBEAT_MS,
    SHARED_WORKER_INVALIDATE_TRANSACTIONS,
    SHARED_WORKER_LEASE_MS,
    SHARED_WORKER_OWNER_REQUEST,
    SHARED_WORKER_OWNER_RESPONSE,
    SHARED_WORKER_OWNER_HOST_MESSAGE,
    type SharedWorkerControlOperation,
    type SharedWorkerControlRequest
} from './shared-worker-protocol';

function decodeControlError(error: unknown): Error {
    try {
        return deserializeWorkerError(error);
    } catch {
        return workerProtocolError('WorkerProtocolError', 'invalid shared worker error');
    }
}

// The page retains this worker while other clients still use its database.
function hostStorageWorker(): { workerPort: MessagePort; hostPort: MessagePort } {
    const worker = new Worker(new URL('./browser-worker.ts', import.meta.url), { type: 'module' });
    const storage = new MessageChannel();
    const host = new MessageChannel();
    const persistence = new MessageChannel();
    const heartbeat = setInterval(() => {
        host.port1.postMessage({ type: SHARED_WORKER_OWNER_HOST_MESSAGE, op: 'ping' });
    }, SHARED_WORKER_HEARTBEAT_MS);
    let stopped = false;
    const stop = () => {
        if (stopped) return;
        stopped = true;
        clearInterval(heartbeat);
        worker.terminate();
        host.port1.close();
        persistence.port1.close();
        globalThis.removeEventListener?.('pagehide', handlePageHide);
    };
    const handlePageHide = () => {
        host.port1.postMessage({ type: SHARED_WORKER_OWNER_HOST_MESSAGE, op: 'gone' });
        stop();
    };
    worker.addEventListener('error', (event: ErrorEvent) => {
        host.port1.postMessage({
            type: SHARED_WORKER_OWNER_HOST_MESSAGE,
            op: 'error',
            error: serializeWorkerError(event.error ?? workerProtocolError('WorkerError', event.message))
        });
        stop();
    });
    host.port1.addEventListener('message', (event: MessageEvent<unknown>) => {
        if (isRecord(event.data) && event.data.type === SHARED_WORKER_OWNER_HOST_MESSAGE && event.data.op === 'stop')
            stop();
    });
    host.port1.start();
    persistence.port1.addEventListener('message', (event: MessageEvent<unknown>) => {
        const data = event.data;
        if (!isRecord(data) || data.type !== 'moyodb:persistence-bridge:request' || typeof data.id !== 'number') return;
        const operation = data.op === 'persist' ? navigator.storage.persist?.() : navigator.storage.persisted?.();
        void withTimeout(Promise.resolve(operation ?? false), 1000, false).then((granted) => {
            if (!stopped)
                persistence.port1.postMessage({ type: 'moyodb:persistence-bridge:response', id: data.id, granted });
        });
    });
    persistence.port1.start();
    globalThis.addEventListener?.('pagehide', handlePageHide);
    worker.postMessage({ type: 'moyodb:worker-port:init' }, [storage.port2]);
    worker.postMessage({ type: 'moyodb:persistence-bridge:init' }, [persistence.port2]);
    return { workerPort: storage.port1, hostPort: host.port2 };
}

export class SharedWorkerProtocolClient extends WorkerProtocolClient {
    private controlId = 1;
    private controls = new Map<number, { resolve: () => void; reject: (reason: Error) => void }>();
    private invalidationHandler: (() => void) | null = null;
    private heartbeat: ReturnType<typeof setInterval> | null;
    private lastPong = Date.now();
    private disposed = false;
    private detachId: number | null = null;
    private detachPromise: Promise<void> | null = null;
    private resolveDetach: (() => void) | null = null;

    constructor(private readonly sharedWorker: SharedWorker) {
        super(sharedWorker.port, { readyTimeoutMs: 10_000 });
        sharedWorker.port.addEventListener('message', this.handleControl);
        sharedWorker.addEventListener('error', this.handleSharedError);
        sharedWorker.port.start();
        globalThis.addEventListener?.('pagehide', this.handlePageHide);
        this.heartbeat = setInterval(() => {
            if (Date.now() - this.lastPong > SHARED_WORKER_LEASE_MS) {
                this.fail(workerProtocolError('WorkerTerminatedError', 'shared worker heartbeat expired'));
                return;
            }
            this.sendControl('ping', 0);
        }, SHARED_WORKER_HEARTBEAT_MS);
    }

    acquireMigration(): Promise<void> {
        return this.control('acquireMigration');
    }

    releaseMigration(): Promise<void> {
        return this.control('releaseMigration');
    }

    setTransactionsInvalidatedHandler(handler: (() => void) | null): void {
        this.invalidationHandler = handler;
    }

    crash(): void {
        this.sendControl('crash', 0);
    }

    override dispose(reason: Error = workerProtocolError('WorkerTerminatedError', 'shared worker port closed')): void {
        this.cleanup(reason);
        super.dispose(reason);
    }

    async disconnect(): Promise<void> {
        this.dispose();
        await this.detachPromise;
        this.sharedWorker.port.close();
    }

    protected override fail(reason: Error): void {
        this.cleanup(reason);
        super.fail(reason);
    }

    private async control(op: SharedWorkerControlOperation): Promise<void> {
        await this.whenReady();
        if (this.disposed) throw workerProtocolError('WorkerTerminatedError', 'shared worker port closed');
        const id = this.controlId++;
        return new Promise<void>((resolve, reject) => {
            this.controls.set(id, { resolve, reject });
            try {
                this.sendControl(op, id);
            } catch (error) {
                this.controls.delete(id);
                reject(error instanceof Error ? error : new Error(String(error)));
            }
        });
    }

    private sendControl(op: SharedWorkerControlOperation, id: number): void {
        const request: SharedWorkerControlRequest = { type: SHARED_WORKER_CONTROL, op, id };
        this.sharedWorker.port.postMessage(request);
    }

    private handleControl = (event: MessageEvent<unknown>): void => {
        const data = event.data;
        if (!isRecord(data)) return;
        if (this.disposed) {
            if (data.type === SHARED_WORKER_CONTROL_RESULT && data.id === this.detachId) this.resolveDetach?.();
            return;
        }
        if (data.type === SHARED_WORKER_OWNER_REQUEST && typeof data.id === 'number') {
            try {
                const { workerPort, hostPort } = hostStorageWorker();
                this.sharedWorker.port.postMessage({ type: SHARED_WORKER_OWNER_RESPONSE, id: data.id, ok: true }, [
                    workerPort,
                    hostPort
                ]);
            } catch (error) {
                this.sharedWorker.port.postMessage({
                    type: SHARED_WORKER_OWNER_RESPONSE,
                    id: data.id,
                    ok: false,
                    error: serializeWorkerError(error)
                });
            }
            return;
        }
        if (data.type === SHARED_WORKER_FATAL) {
            this.fail(decodeControlError(data.error));
            return;
        }
        if (data.type === SHARED_WORKER_INVALIDATE_TRANSACTIONS) {
            this.invalidationHandler?.();
            return;
        }
        if (data.type !== SHARED_WORKER_CONTROL_RESULT) return;
        if (data.id === 0 && data.ok === true) {
            this.lastPong = Date.now();
            return;
        }
        if (typeof data.id !== 'number') return;
        const pending = this.controls.get(data.id);
        if (!pending) return;
        this.controls.delete(data.id);
        if (data.ok === true) pending.resolve();
        else pending.reject(decodeControlError(data.error));
    };

    private handleSharedError = (event: ErrorEvent): void => {
        this.fail(workerProtocolError('WorkerError', event.message || 'shared worker runtime failed'));
    };

    private handlePageHide = (): void => {
        this.fail(workerProtocolError('WorkerTerminatedError', 'shared worker page was unloaded'));
    };

    private cleanup(reason: Error): void {
        if (this.disposed) return;
        this.disposed = true;
        if (this.heartbeat !== null) clearInterval(this.heartbeat);
        this.heartbeat = null;
        this.detachId = this.controlId++;
        // Closing the port before this acknowledgement can discard the queued detach message.
        const detached = new Promise<void>((resolve) => {
            this.resolveDetach = resolve;
        });
        this.detachPromise = withTimeout(detached, 1000, undefined).then(() => {
            this.resolveDetach = null;
            this.sharedWorker.port.removeEventListener('message', this.handleControl);
        });
        try {
            this.sendControl('detach', this.detachId);
        } catch {
            this.resolveDetach?.();
        }
        this.sharedWorker.removeEventListener('error', this.handleSharedError);
        globalThis.removeEventListener?.('pagehide', this.handlePageHide);
        for (const pending of this.controls.values()) pending.reject(reason);
        this.controls.clear();
        this.invalidationHandler = null;
    }
}
