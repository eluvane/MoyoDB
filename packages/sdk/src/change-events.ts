import type { DbChange } from './types';
import { isRecord } from './internal';
export interface StoreChangeSet {
    store: string;
    changes: DbChange[];
}
export interface CommitAppliedEvent {
    type: 'commit_applied';
    dbName: string;
    txid: number;
    stores: StoreChangeSet[];
}
export interface OwnerChangedEvent {
    type: 'owner_changed';
    dbName: string;
    txid: null;
}
export interface DbClosedEvent {
    type: 'db_closed';
    dbName: string;
    txid: null;
}
export interface DbDeletedEvent {
    type: 'db_deleted';
    dbName: string;
    txid: null;
}
export type WorkerBroadcastEvent = CommitAppliedEvent | OwnerChangedEvent | DbClosedEvent | DbDeletedEvent;
function isDbChange(value: unknown): value is DbChange {
    return isRecord(value) && value.key instanceof Uint8Array && (value.kind === 'put' || value.kind === 'delete');
}
function isStoreChangeSet(value: unknown): value is StoreChangeSet {
    return (
        isRecord(value) &&
        typeof value.store === 'string' &&
        Array.isArray(value.changes) &&
        value.changes.every(isDbChange)
    );
}
export function isCommitAppliedEvent(value: unknown): value is CommitAppliedEvent {
    return (
        isRecord(value) &&
        value.type === 'commit_applied' &&
        typeof value.dbName === 'string' &&
        typeof value.txid === 'number' &&
        Array.isArray(value.stores) &&
        value.stores.every(isStoreChangeSet)
    );
}
