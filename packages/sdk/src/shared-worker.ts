import type { WorkerApi, WorkerOpenRequest, WorkerScanPageRequest } from './worker-api';
import { WorkerProtocolClient } from './worker-client';
import { exposeWorkerApi, type WorkerServerHandle, type WorkerServerScope } from './worker-server';
import {
    WORKER_COMMANDS,
    deserializeWorkerError,
    serializeWorkerError,
    workerProtocolError,
    type WorkerCommand
} from './worker-protocol';
import { isRecord } from './internal';
import {
    SHARED_WORKER_CONTROL,
    SHARED_WORKER_CONTROL_RESULT,
    SHARED_WORKER_FATAL,
    SHARED_WORKER_HEARTBEAT_MS,
    SHARED_WORKER_INVALIDATE_TRANSACTIONS,
    SHARED_WORKER_LEASE_MS,
    SHARED_WORKER_OWNER_REQUEST,
    SHARED_WORKER_OWNER_RESPONSE,
    SHARED_WORKER_OWNER_HOST_MESSAGE
} from './shared-worker-protocol';

const TRANSACTION_COMMANDS = new Set<WorkerCommand>([
    'commit',
    'rollback',
    'createStore',
    'dropStore',
    'clearStore',
    'get',
    'getMany',
    'has',
    'put',
    'putMany',
    'delete',
    'deleteMany',
    'applyBatch',
    'scan',
    'getByIndex',
    'scanByIndex',
    'scanByIndexPage',
    'reconcileIndexes',
    'setSchemaVersion'
]);
const MAINTENANCE_COMMANDS = new Set<WorkerCommand>(['importSnapshot', 'reset', 'compact', 'rebuild']);

interface Session {
    port: MessagePort;
    server: WorkerServerHandle | null;
    opened: boolean;
    closing: boolean;
    nextTxId: number;
    transactions: Map<number, number>;
    nextCursorId: number;
    cursors: Map<number, { ownerId: number; localTxId?: number }>;
    cursorGeneration: number;
    pending: Set<Promise<unknown>>;
    lastSeen: number;
    closingPromise: Promise<void> | null;
}

interface Owner {
    port: MessagePort;
    hostPort: MessagePort;
    proxy: WorkerProtocolClient;
    request: WorkerOpenRequest;
    session: Session;
    lastSeen: number;
}

interface PendingOwner {
    session: Session;
    resolve: (ports: [MessagePort, MessagePort]) => void;
    reject: (reason: Error) => void;
    timeout: ReturnType<typeof setTimeout>;
}

export class SharedWorkerCoordinator {
    private sessions = new Set<Session>();
    private owner: Owner | null = null;
    private dbName: string | null = null;
    private ownerId = 1;
    private pendingOwners = new Map<number, PendingOwner>();
    private storageReady: Promise<void> = Promise.resolve();
    private lifecycle: Promise<void> = Promise.resolve();
    private migrationOwner: Session | null = null;
    private migrationReleased: (() => void) | null = null;
    private migrationQueue: Promise<void> = Promise.resolve();
    private timer: ReturnType<typeof setInterval> | null = null;

    constructor(private readonly origin: string) {}

    connect(port: MessagePort): void {
        const session: Session = {
            port,
            server: null,
            opened: false,
            closing: false,
            nextTxId: 1,
            transactions: new Map(),
            nextCursorId: 1,
            cursors: new Map(),
            cursorGeneration: 0,
            pending: new Set(),
            lastSeen: Date.now(),
            closingPromise: null
        };
        this.sessions.add(session);
        this.timer ??= setInterval(() => this.expireSessions(), SHARED_WORKER_HEARTBEAT_MS);
        const handleControl = (event: MessageEvent<unknown>) => {
            if (event.origin !== '' && event.origin !== this.origin) return;
            const data = event.data;
            if (!isRecord(data)) return;
            session.lastSeen = Date.now();
            if (data.type === SHARED_WORKER_OWNER_RESPONSE && typeof data.id === 'number') {
                const pending = this.pendingOwners.get(data.id);
                if (!pending || pending.session !== session) {
                    event.ports[1]?.postMessage({ type: SHARED_WORKER_OWNER_HOST_MESSAGE, op: 'stop' });
                    for (const transferredPort of event.ports) transferredPort.close();
                    return;
                }
                this.pendingOwners.delete(data.id);
                clearTimeout(pending.timeout);
                if (data.ok === true && event.ports.length === 2) pending.resolve([event.ports[0], event.ports[1]]);
                else pending.reject(this.controlError(data.error));
                return;
            }
            if (data.type !== SHARED_WORKER_CONTROL || typeof data.id !== 'number') return;
            const reply = (error?: unknown) => {
                port.postMessage({
                    type: SHARED_WORKER_CONTROL_RESULT,
                    id: data.id,
                    ok: error === undefined,
                    ...(error === undefined ? {} : { error: serializeWorkerError(error) })
                });
            };
            if (data.op === 'ping') {
                reply();
            } else if (data.op === 'detach') {
                void this.closeSession(session).then(
                    () => reply(),
                    (error) => {
                        reply(error);
                        this.failAll(error);
                    }
                );
            } else if (data.op === 'crash') {
                this.failAll(
                    workerProtocolError(
                        'WorkerTerminatedError',
                        'storage worker was terminated by unsafeDebugCrashWorker'
                    )
                );
            } else if (data.op === 'acquireMigration') {
                void this.acquireMigration(session).then(() => reply(), reply);
            } else if (data.op === 'releaseMigration') {
                this.releaseMigration(session);
                reply();
            } else {
                reply(workerProtocolError('WorkerProtocolError', 'unsupported shared worker control'));
            }
        };
        port.addEventListener('message', handleControl);
        port.addEventListener('messageerror', () => {
            void this.closeSession(session).catch((error) => this.failAll(error));
        });
        const scope: WorkerServerScope = {
            location: { origin: this.origin },
            postMessage: (message, transfer) => port.postMessage(message, transfer ?? []),
            addEventListener: (type, listener) => port.addEventListener(type, listener),
            removeEventListener: (type, listener) => port.removeEventListener(type, listener)
        };
        session.server = exposeWorkerApi(this.sessionApi(session), scope);
        port.start();
    }

    private sessionApi(session: Session): WorkerApi {
        const methods: Record<string, (...args: unknown[]) => Promise<unknown>> = {};
        for (const command of WORKER_COMMANDS) {
            methods[command] = (...args) => this.invoke(session, command, args);
        }
        return methods as unknown as WorkerApi;
    }

    private invoke(session: Session, command: WorkerCommand, args: unknown[]): Promise<unknown> {
        if (command === 'open')
            return this.queueLifecycle(() => this.openSession(session, args[0] as WorkerOpenRequest));
        if (command === 'close') return this.closeSession(session);
        if (command === 'destroy') return this.queueLifecycle(() => this.destroySession(session));
        if (command === 'deleteDB') {
            return Promise.reject(
                workerProtocolError('DatabaseBusyError', 'delete shared databases through an exclusive open handle')
            );
        }
        const operation = (async () => {
            await this.storageReady;
            // Existing transactions must be able to finish during another client's schema lease.
            this.requireSession(
                session,
                command !== 'begin' && command !== 'setFailpoint' && !MAINTENANCE_COMMANDS.has(command)
            );
            const owner = this.owner!;
            if (command === 'begin') {
                if (
                    args[0] === 'readwrite' &&
                    this.migrationOwner === session &&
                    Array.from(this.sessions).some(
                        (client) => client !== session && (client.transactions.size > 0 || client.cursors.size > 0)
                    )
                ) {
                    throw workerProtocolError(
                        'DatabaseBusyError',
                        'close active shared transactions before schema migration'
                    );
                }
                const ownerTxId = await owner.proxy.begin(args[0] as 'readonly' | 'readwrite');
                const localTxId = session.nextTxId++;
                session.transactions.set(localTxId, ownerTxId);
                return localTxId;
            }
            if (command === 'scanPage') {
                if (!isRecord(args[0])) throw workerProtocolError('WorkerProtocolError', 'invalid scan page request');
                const request = args[0] as unknown as WorkerScanPageRequest;
                const localCursorId = request.cursorId;
                const cursor = localCursorId === undefined ? undefined : session.cursors.get(localCursorId);
                if (localCursorId !== undefined && !cursor) {
                    throw workerProtocolError(
                        'TransactionClosedError',
                        'scan cursor does not belong to this shared client'
                    );
                }
                if (cursor && cursor.localTxId !== request.txId) {
                    throw workerProtocolError(
                        'TransactionClosedError',
                        'scan cursor does not belong to this transaction'
                    );
                }
                const ownerTxId = request.txId === undefined ? undefined : session.transactions.get(request.txId);
                if (request.txId !== undefined && ownerTxId === undefined) {
                    throw workerProtocolError(
                        'TransactionClosedError',
                        'transaction does not belong to this shared client'
                    );
                }
                const generation = session.cursorGeneration;
                const page = await owner.proxy.scanPage({ ...request, txId: ownerTxId, cursorId: cursor?.ownerId });
                if (generation !== session.cursorGeneration || this.owner !== owner) {
                    if (page.cursorId !== undefined && this.owner === owner)
                        await owner.proxy.closeCursor(page.cursorId);
                    throw workerProtocolError('TransactionClosedError', 'scan snapshot was invalidated');
                }
                if (localCursorId !== undefined) session.cursors.delete(localCursorId);
                if (page.cursorId === undefined) return page;
                const nextCursorId = localCursorId ?? session.nextCursorId++;
                session.cursors.set(nextCursorId, { ownerId: page.cursorId, localTxId: request.txId });
                return { ...page, cursorId: nextCursorId };
            }
            if (command === 'closeCursor') {
                const cursor = session.cursors.get(args[0] as number);
                if (!cursor) {
                    throw workerProtocolError(
                        'TransactionClosedError',
                        'scan cursor does not belong to this shared client'
                    );
                }
                try {
                    await owner.proxy.closeCursor(cursor.ownerId);
                } finally {
                    session.cursors.delete(args[0] as number);
                }
                return;
            }
            if (TRANSACTION_COMMANDS.has(command) || (command === 'getIndexes' && args[0] !== undefined)) {
                const localTxId = args[0] as number;
                const ownerTxId = session.transactions.get(localTxId);
                if (ownerTxId === undefined) {
                    throw workerProtocolError(
                        'TransactionClosedError',
                        'transaction does not belong to this shared client'
                    );
                }
                try {
                    return await owner.proxy.request(command, [ownerTxId, ...args.slice(1)] as never);
                } finally {
                    if (command === 'commit' || command === 'rollback') {
                        session.transactions.delete(localTxId);
                        for (const [cursorId, cursor] of session.cursors) {
                            if (cursor.localTxId === localTxId) session.cursors.delete(cursorId);
                        }
                    }
                }
            }
            if (MAINTENANCE_COMMANDS.has(command)) {
                this.invalidateTransactions();
            }
            return owner.proxy.request(command, args as never);
        })();
        session.pending.add(operation);
        void operation.then(
            () => session.pending.delete(operation),
            () => session.pending.delete(operation)
        );
        return operation;
    }

    private requireSession(session: Session, allowDuringMigration = false): void {
        if (!session.opened || session.closing || !this.owner) {
            throw workerProtocolError('DatabaseClosedError', 'shared client is closed');
        }
        if (!allowDuringMigration && this.migrationOwner !== null && this.migrationOwner !== session) {
            throw workerProtocolError(
                'DatabaseBusyError',
                'database schema migration is owned by another shared client'
            );
        }
    }

    private queueLifecycle<T>(operation: () => Promise<T>): Promise<T> {
        const result = this.lifecycle.then(operation);
        this.lifecycle = result.then(
            () => undefined,
            () => undefined
        );
        return result;
    }

    private async openSession(session: Session, request: WorkerOpenRequest): Promise<void> {
        if (session.closing || !this.sessions.has(session))
            throw workerProtocolError('DatabaseClosedError', 'shared client is closed');
        if (session.opened) throw workerProtocolError('InvalidOpenOptionsError', 'shared client is already open');
        if (!isRecord(request) || typeof request.dbName !== 'string' || !isRecord(request.options)) {
            throw workerProtocolError('InvalidOpenOptionsError', 'invalid shared worker open request');
        }
        if (this.dbName !== null && this.dbName !== request.dbName) {
            throw workerProtocolError('InvalidOpenOptionsError', 'shared worker database name does not match');
        }
        this.dbName = request.dbName;
        if (this.owner) {
            const current = this.owner.request;
            if (request.dbName !== current.dbName)
                throw workerProtocolError('InvalidOpenOptionsError', 'shared worker database name does not match');
            if (request.options.cachePages !== current.options.cachePages) {
                throw workerProtocolError(
                    'InvalidOpenOptionsError',
                    'shared database cachePages policy does not match its owner'
                );
            }
            const feed = request.options.changeFeed;
            if (
                feed !== null &&
                (feed.enabled !== current.options.changeFeed?.enabled ||
                    feed.retainTxids !== current.options.changeFeed?.retainTxids)
            ) {
                throw workerProtocolError(
                    'InvalidOpenOptionsError',
                    'shared database changeFeed policy does not match its owner'
                );
            }
            if (request.options.debugFailpoint !== null) {
                if (this.migrationOwner !== null && this.migrationOwner !== session) {
                    throw workerProtocolError(
                        'DatabaseBusyError',
                        'cannot change failpoints during another client schema migration'
                    );
                }
                await this.owner.proxy.setFailpoint(request.options.debugFailpoint);
            }
            session.opened = true;
            return;
        }
        const owner = await this.createOwner(session, request);
        this.owner = owner;
        session.opened = true;
        try {
            await owner.proxy.open(request);
        } catch (error) {
            session.opened = false;
            this.disposeOwner(owner);
            throw error;
        }
    }

    private async createOwner(session: Session, request: WorkerOpenRequest): Promise<Owner> {
        const id = this.ownerId++;
        const [port, hostPort] = await new Promise<[MessagePort, MessagePort]>((resolve, reject) => {
            const timeout = setTimeout(() => {
                this.pendingOwners.delete(id);
                reject(
                    workerProtocolError('WorkerReadyTimeoutError', 'shared storage owner did not provide a worker port')
                );
            }, 10_000);
            this.pendingOwners.set(id, { session, resolve, reject, timeout });
            session.port.postMessage({ type: SHARED_WORKER_OWNER_REQUEST, id });
        });
        const proxy = new WorkerProtocolClient(port, { readyTimeoutMs: 10_000, forwardScanPackets: true });
        const owner: Owner = { port, hostPort, proxy, request, session, lastSeen: Date.now() };
        proxy.setFatalHandler((error) => this.failAll(error));
        hostPort.addEventListener('message', (event: MessageEvent<unknown>) => {
            const data = event.data;
            if (!isRecord(data) || data.type !== SHARED_WORKER_OWNER_HOST_MESSAGE || this.owner !== owner) return;
            owner.lastSeen = Date.now();
            if (data.op === 'gone') this.recoverOwner(owner);
            else if (data.op === 'error') this.failAll(this.controlError(data.error));
        });
        hostPort.start();
        port.start();
        return owner;
    }

    private closeSession(session: Session): Promise<void> {
        if (session.closingPromise) return session.closingPromise;
        session.closing = true;
        session.closingPromise = this.queueLifecycle(async () => {
            await Promise.allSettled(Array.from(session.pending));
            const owner = this.owner;
            if (owner && session.opened) {
                const results = await Promise.allSettled([
                    ...Array.from(session.cursors.values(), (cursor) => owner.proxy.closeCursor(cursor.ownerId)),
                    ...Array.from(session.transactions.values(), (id) => owner.proxy.rollback(id))
                ]);
                const failure = results.find(
                    (result) => result.status === 'rejected' && result.reason?.name !== 'TransactionClosedError'
                );
                if (failure?.status === 'rejected') {
                    this.failAll(failure.reason);
                    throw failure.reason;
                }
            }
            session.transactions.clear();
            session.cursors.clear();
            session.cursorGeneration += 1;
            session.opened = false;
            this.releaseMigration(session);
            session.server?.close();
            this.sessions.delete(session);
            if (owner && !Array.from(this.sessions).some((client) => client.opened)) {
                try {
                    await owner.proxy.close();
                } finally {
                    this.disposeOwner(owner);
                }
            }
            if (this.sessions.size === 0 && this.timer !== null) {
                clearInterval(this.timer);
                this.timer = null;
            }
        });
        return session.closingPromise;
    }

    private async destroySession(session: Session): Promise<void> {
        await this.storageReady;
        this.requireSession(session);
        if (Array.from(this.sessions).some((client) => client !== session && client.opened)) {
            throw workerProtocolError('DatabaseBusyError', 'cannot destroy a database with other live shared clients');
        }
        const owner = this.owner!;
        await owner.proxy.destroy();
        this.invalidateTransactions();
        session.opened = false;
        this.releaseMigration(session);
        this.disposeOwner(owner);
    }

    private acquireMigration(session: Session): Promise<void> {
        const previous = this.migrationQueue;
        let release!: () => void;
        const released = new Promise<void>((resolve) => {
            release = resolve;
        });
        this.migrationQueue = previous.then(() => released);
        return previous.then(async () => {
            try {
                await this.storageReady;
                this.requireSession(session);
                this.migrationOwner = session;
                this.migrationReleased = release;
                await Promise.allSettled(Array.from(this.sessions).flatMap((client) => Array.from(client.pending)));
                this.requireSession(session);
            } catch (error) {
                if (this.migrationOwner === session) this.releaseMigration(session);
                else release();
                throw error;
            }
        });
    }

    private releaseMigration(session: Session): void {
        if (this.migrationOwner !== session) return;
        this.migrationOwner = null;
        const release = this.migrationReleased;
        this.migrationReleased = null;
        release?.();
    }

    private invalidateTransactions(): void {
        for (const session of this.sessions) {
            session.transactions.clear();
            session.cursors.clear();
            session.cursorGeneration += 1;
            if (session.opened) session.port.postMessage({ type: SHARED_WORKER_INVALIDATE_TRANSACTIONS });
        }
    }

    private recoverOwner(owner: Owner): void {
        // A dedicated worker ends with its hosting page. Its transaction snapshots cannot survive.
        this.disposeOwner(owner);
        this.invalidateTransactions();
        const recovering = this.queueLifecycle(async () => {
            if (this.owner) return;
            const next = Array.from(this.sessions).find(
                (session) =>
                    session !== owner.session &&
                    session.opened &&
                    !session.closing &&
                    Date.now() - session.lastSeen <= SHARED_WORKER_LEASE_MS
            );
            if (!next) return;
            const request: WorkerOpenRequest = {
                dbName: owner.request.dbName,
                options: {
                    ...owner.request.options,
                    createIfMissing: false,
                    ownerWaitMs: Math.max(owner.request.options.ownerWaitMs, 2000),
                    debugFailpoint: null
                }
            };
            const replacement = await this.createOwner(next, request);
            this.owner = replacement;
            await replacement.proxy.open(request);
        });
        this.storageReady = recovering;
        void recovering.catch((error) => this.failAll(error));
    }

    private expireSessions(): void {
        if (this.owner && Date.now() - this.owner.lastSeen > SHARED_WORKER_LEASE_MS) {
            this.recoverOwner(this.owner);
        }
        for (const session of this.sessions) {
            if (Date.now() - session.lastSeen > SHARED_WORKER_LEASE_MS) {
                session.port.postMessage({
                    type: SHARED_WORKER_FATAL,
                    error: serializeWorkerError(
                        workerProtocolError('WorkerTerminatedError', 'shared client heartbeat expired')
                    )
                });
                void this.closeSession(session).catch((error) => this.failAll(error));
            }
        }
    }

    private disposeOwner(owner: Owner): void {
        if (this.owner === owner) this.owner = null;
        owner.proxy.dispose();
        owner.hostPort.postMessage({ type: SHARED_WORKER_OWNER_HOST_MESSAGE, op: 'stop' });
        owner.hostPort.close();
        owner.port.close();
    }

    private controlError(error: unknown): Error {
        try {
            return deserializeWorkerError(error);
        } catch {
            return workerProtocolError('WorkerProtocolError', 'invalid shared worker owner response');
        }
    }

    private failAll(error: unknown): void {
        if (this.owner) this.disposeOwner(this.owner);
        for (const pending of this.pendingOwners.values()) {
            clearTimeout(pending.timeout);
            pending.reject(error instanceof Error ? error : new Error(String(error)));
        }
        this.pendingOwners.clear();
        for (const session of this.sessions) {
            session.port.postMessage({ type: SHARED_WORKER_FATAL, error: serializeWorkerError(error) });
            session.opened = false;
            session.closing = true;
            session.transactions.clear();
            session.cursors.clear();
            session.cursorGeneration += 1;
            session.server?.close();
            this.releaseMigration(session);
        }
        this.sessions.clear();
        this.storageReady = Promise.resolve();
        if (this.timer !== null) clearInterval(this.timer);
        this.timer = null;
    }
}

if (typeof self !== 'undefined' && 'onconnect' in self) {
    const scope = self as SharedWorkerGlobalScope;
    const coordinator = new SharedWorkerCoordinator(scope.location.origin);
    scope.addEventListener('connect', (event: MessageEvent) => {
        for (const port of event.ports) coordinator.connect(port);
    });
}
