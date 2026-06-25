import { DatabaseClosedError } from './errors';
import { isCommitAppliedEvent } from './change-events';
import { assertPublicStoreName } from './indexing';
import type { DbChange, DbSubscriptionCallback, Unsubscribe } from './types';
interface SubscriptionEntry {
    active: boolean;
    store: string | null;
    keyPrefix: Uint8Array | null;
    callback: DbSubscriptionCallback;
}
interface SubscriptionSpec {
    store: string | null;
    keyPrefix: Uint8Array | null;
    callback: DbSubscriptionCallback;
}
function cloneChanges(changes: readonly DbChange[]): DbChange[] {
    return changes.map((change) => ({
        key: change.key.slice(),
        kind: change.kind
    }));
}
function hasKeyPrefix(key: Uint8Array, prefix: Uint8Array): boolean {
    if (prefix.byteLength > key.byteLength) {
        return false;
    }
    for (let index = 0; index < prefix.byteLength; index += 1) {
        if (key[index] !== prefix[index]) {
            return false;
        }
    }
    return true;
}
function normalizeSubscriptionSpec(
    arg1: string | DbSubscriptionCallback,
    arg2?: Uint8Array | DbSubscriptionCallback,
    arg3?: DbSubscriptionCallback
): SubscriptionSpec {
    if (typeof arg1 === 'function') {
        if (arg2 !== undefined || arg3 !== undefined) {
            throw new TypeError('subscribe(callback) accepts exactly one callback argument');
        }
        return {
            store: null,
            keyPrefix: null,
            callback: arg1
        };
    }
    if (typeof arg1 !== 'string') {
        throw new TypeError('subscribe() storeName must be a string');
    }
    assertPublicStoreName(arg1);
    if (typeof arg2 === 'function') {
        if (arg3 !== undefined) {
            throw new TypeError('subscribe(storeName, callback) accepts exactly two arguments');
        }
        return {
            store: arg1,
            keyPrefix: null,
            callback: arg2
        };
    }
    if (arg2 instanceof Uint8Array && typeof arg3 === 'function') {
        return {
            store: arg1,
            keyPrefix: arg2.slice(),
            callback: arg3
        };
    }
    throw new TypeError(
        'subscribe() expects one of: (callback), (storeName, callback), (storeName, keyPrefix, callback)'
    );
}
export class SubscriptionHub {
    #dbName: string;
    #channel: BroadcastChannel | null = null;
    #entries = new Set<SubscriptionEntry>();
    #closed = false;
    constructor(dbName: string) {
        this.#dbName = dbName;
    }
    subscribe(callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(storeName: string, callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(storeName: string, keyPrefix: Uint8Array, callback: DbSubscriptionCallback): Unsubscribe;
    subscribe(
        arg1: string | DbSubscriptionCallback,
        arg2?: Uint8Array | DbSubscriptionCallback,
        arg3?: DbSubscriptionCallback
    ): Unsubscribe {
        if (this.#closed) {
            throw new DatabaseClosedError();
        }
        const spec = normalizeSubscriptionSpec(arg1, arg2, arg3);
        const entry: SubscriptionEntry = {
            active: true,
            store: spec.store,
            keyPrefix: spec.keyPrefix,
            callback: spec.callback
        };
        this.#ensureChannel();
        this.#entries.add(entry);
        return () => {
            if (!entry.active) {
                return;
            }
            entry.active = false;
            this.#entries.delete(entry);
            if (this.#entries.size === 0) {
                this.#disposeChannel();
            }
        };
    }
    close(): void {
        if (this.#closed) {
            return;
        }
        this.#closed = true;
        this.#entries.clear();
        this.#disposeChannel();
    }
    #ensureChannel() {
        if (this.#channel !== null || this.#closed) {
            return;
        }
        const channel = new BroadcastChannel(`db:${this.#dbName}:events`);
        channel.onmessage = (event) => {
            this.#handleMessage(event.data);
        };
        this.#channel = channel;
    }
    #disposeChannel() {
        this.#channel?.close();
        this.#channel = null;
    }
    #handleMessage(payload: unknown) {
        if (!isCommitAppliedEvent(payload) || payload.dbName !== this.#dbName || this.#entries.size === 0) {
            return;
        }
        const entries = Array.from(this.#entries);
        for (const storeEvent of payload.stores) {
            for (const entry of entries) {
                if (!entry.active) {
                    continue;
                }
                if (entry.store !== null && entry.store !== storeEvent.store) {
                    continue;
                }
                let changes: DbChange[];
                if (entry.keyPrefix === null) {
                    changes = cloneChanges(storeEvent.changes);
                } else {
                    changes = cloneChanges(
                        storeEvent.changes.filter((change) => hasKeyPrefix(change.key, entry.keyPrefix!))
                    );
                    if (changes.length === 0) {
                        continue;
                    }
                }
                try {
                    entry.callback(storeEvent.store, changes, payload.txid);
                } catch (error) {
                    queueMicrotask(() => {
                        throw error;
                    });
                }
            }
        }
    }
}
