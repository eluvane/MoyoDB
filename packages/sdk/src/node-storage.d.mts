export const MISSING_DATABASE_MESSAGE: string;

export interface NodeStorageOptions {
    lockToken?: string;
    encodedDbName?: string;
    createIfMissing?: boolean;
}

export interface NodeStorageHandle {
    directory: string;
    close(): void;
}

export function ensureStorageRoot(directory: string, createIfMissing?: boolean): string;
export function installNodeStorage(directory: string, options?: NodeStorageOptions): Promise<NodeStorageHandle>;
export function releaseNodeStorageLease(directory: string, lockToken: string): boolean;
