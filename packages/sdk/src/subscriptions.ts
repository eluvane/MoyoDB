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
interface PendingDelivery {
    entry: SubscriptionEntry;
    changes: DbChange[];
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
function matchesPrefix(change: DbChange, prefix: Uint8Array): boolean {
    return change.kind === 'clear' || change.kind === 'drop' || hasKeyPrefix(change.key, prefix);
}
function includesStoreLevelChange(changes: readonly DbChange[]): boolean {
    for (let index = 0; index < changes.length; index += 1) {
        const kind = changes[index].kind;
        if (kind === 'clear' || kind === 'drop') {
            return true;
        }
    }
    return false;
}
function prefixBytesOverlap(left: Uint8Array, right: Uint8Array): boolean {
    const limit = Math.min(left.byteLength, right.byteLength);
    for (let index = 0; index < limit; index += 1) {
        if (left[index] !== right[index]) {
            return false;
        }
    }
    return true;
}
function sharedChanges(changes: DbChange[], prefix: Uint8Array | null): DbChange[] | null {
    if (prefix === null) {
        return changes;
    }
    let selected: DbChange[] | null = null;
    for (let index = 0; index < changes.length; index += 1) {
        const change = changes[index];
        if (matchesPrefix(change, prefix)) {
            if (selected !== null) {
                selected.push(change);
            }
            continue;
        }
        if (selected === null) {
            selected = changes.slice(0, index);
        }
    }
    if (selected === null) {
        return changes.length === 0 ? null : changes;
    }
    return selected.length === 0 ? null : selected;
}
function viewsIntersect(
    leftPrefix: Uint8Array | null,
    rightPrefix: Uint8Array | null,
    hasStoreLevel: () => boolean
): boolean {
    if (leftPrefix === null || rightPrefix === null) {
        return true;
    }
    if (prefixBytesOverlap(leftPrefix, rightPrefix)) {
        return true;
    }
    return hasStoreLevel();
}
function laterViewIntersects(
    deliveries: readonly PendingDelivery[],
    index: number,
    hasStoreLevel: () => boolean
): boolean {
    const prefix = deliveries[index].entry.keyPrefix;
    for (let later = index + 1; later < deliveries.length; later += 1) {
        if (viewsIntersect(prefix, deliveries[later].entry.keyPrefix, hasStoreLevel)) {
            return true;
        }
    }
    return false;
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
    #channelName: string;
    #channel: BroadcastChannel | null = null;
    #entries = new Set<SubscriptionEntry>();
    #closed = false;
    constructor(dbName: string, channelName = dbName) {
        this.#dbName = dbName;
        this.#channelName = channelName;
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
        for (const entry of this.#entries) {
            entry.active = false;
        }
        this.#entries.clear();
        this.#disposeChannel();
    }
    #ensureChannel() {
        if (this.#channel !== null || this.#closed) {
            return;
        }
        const channel = new BroadcastChannel(`db:${this.#channelName}:events`);
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
            const deliveries: PendingDelivery[] = [];
            for (const entry of entries) {
                if (!entry.active) {
                    continue;
                }
                if (entry.store !== null && entry.store !== storeEvent.store) {
                    continue;
                }
                const changes = sharedChanges(storeEvent.changes, entry.keyPrefix);
                if (changes === null) {
                    continue;
                }
                deliveries.push({ entry, changes });
            }
            // The last listener that shares a view reuses the commit's objects.
            // Earlier listeners get private copies before any callback runs.
            if (deliveries.length > 1) {
                let storeLevel: boolean | undefined;
                const hasStoreLevel = (): boolean => {
                    if (storeLevel === undefined) {
                        storeLevel = includesStoreLevelChange(storeEvent.changes);
                    }
                    return storeLevel;
                };
                for (let index = 0; index < deliveries.length; index += 1) {
                    if (!laterViewIntersects(deliveries, index, hasStoreLevel)) {
                        continue;
                    }
                    deliveries[index].changes = cloneChanges(deliveries[index].changes);
                }
            }
            for (const delivery of deliveries) {
                if (!delivery.entry.active) {
                    continue;
                }
                try {
                    delivery.entry.callback(storeEvent.store, delivery.changes, payload.txid);
                } catch (error) {
                    queueMicrotask(() => {
                        throw error;
                    });
                }
            }
        }
    }
}
