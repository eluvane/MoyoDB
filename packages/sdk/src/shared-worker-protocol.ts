export const SHARED_WORKER_CONTROL = 'moyodb:shared-worker:control';
export const SHARED_WORKER_CONTROL_RESULT = 'moyodb:shared-worker:control-result';
export const SHARED_WORKER_FATAL = 'moyodb:shared-worker:fatal';
export const SHARED_WORKER_INVALIDATE_TRANSACTIONS = 'moyodb:shared-worker:invalidate-transactions';
export const SHARED_WORKER_OWNER_REQUEST = 'moyodb:shared-worker:owner-request';
export const SHARED_WORKER_OWNER_RESPONSE = 'moyodb:shared-worker:owner-response';
export const SHARED_WORKER_OWNER_HOST_MESSAGE = 'moyodb:shared-worker:owner-host-message';
export const SHARED_WORKER_HEARTBEAT_MS = 5_000;
export const SHARED_WORKER_LEASE_MS = 120_000;

export type SharedWorkerControlOperation = 'ping' | 'detach' | 'acquireMigration' | 'releaseMigration' | 'crash';

export interface SharedWorkerControlRequest {
    type: typeof SHARED_WORKER_CONTROL;
    id: number;
    op: SharedWorkerControlOperation;
}
