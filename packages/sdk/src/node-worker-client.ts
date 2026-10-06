import { Worker, type Transferable as NodeTransferable } from 'node:worker_threads';
import { WorkerProtocolClient } from './worker-client';
import { workerProtocolError } from './worker-protocol';

export interface NodeWorkerData {
    directory: string;
    dbName: string;
    channelName: string;
    lockToken: string;
    ownerWaitMs: number;
    encodedDbName: string;
    createIfMissing: boolean;
}

export class NodeWorkerTransport {
    readonly proxy: WorkerProtocolClient;
    readonly #worker: Worker;
    readonly #listeners = new Map<string, Set<EventListenerOrEventListenerObject>>();
    readonly #exited: Promise<number>;
    #resolveExit!: (code: number) => void;
    #rejectExit!: (error: unknown) => void;
    #terminating = false;
    #failureEmitted = false;

    constructor(data: NodeWorkerData, releaseLease: () => void) {
        this.#exited = new Promise((resolve, reject) => {
            this.#resolveExit = resolve;
            this.#rejectExit = reject;
        });
        void this.#exited.catch(() => {});
        this.#worker = new Worker(new URL('./node-worker.mjs', import.meta.url), { workerData: data });
        this.#worker.on('message', (data: unknown) => {
            this.#emit('message', { data });
        });
        this.#worker.on('messageerror', () => {
            this.#emit('messageerror', {});
        });
        this.#worker.on('error', (error: Error) => {
            this.#failureEmitted = true;
            this.#emit('error', { error, message: error.message });
        });
        this.#worker.once('exit', (code: number) => {
            if (!this.#terminating && !this.#failureEmitted) {
                const error = workerProtocolError('WorkerTerminatedError', `Node worker exited with code ${code}`);
                this.#emit('error', { error, message: error.message });
            }
            try {
                releaseLease();
                this.#resolveExit(code);
            } catch (error) {
                this.#rejectExit(error);
            }
        });
        this.proxy = new WorkerProtocolClient(this, {
            readyTimeoutMs: Math.min(2_147_483_647, 30_000 + data.ownerWaitMs)
        });
    }

    postMessage(message: unknown, transfer: Transferable[] = []): void {
        this.#worker.postMessage(message, transfer as NodeTransferable[]);
    }

    addEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
        let listeners = this.#listeners.get(type);
        if (!listeners) {
            listeners = new Set();
            this.#listeners.set(type, listeners);
        }
        listeners.add(listener);
    }

    removeEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
        this.#listeners.get(type)?.delete(listener);
    }

    terminate(): Promise<number> {
        if (!this.#terminating) {
            this.#terminating = true;
            void this.#worker.terminate().catch(this.#rejectExit);
        }
        return this.#exited;
    }

    #emit(type: string, event: unknown): void {
        for (const listener of Array.from(this.#listeners.get(type) ?? [])) {
            if (typeof listener === 'function') listener(event as Event);
            else listener.handleEvent(event as Event);
        }
    }
}
