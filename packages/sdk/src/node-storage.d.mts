export interface NodeStorageOptions {
    lockToken?: string;
    encodedDbName?: string;
}

export interface NodeStorageHandle {
    directory: string;
    close(): void;
}

export function installNodeStorage(directory: string, options?: NodeStorageOptions): Promise<NodeStorageHandle>;
export function releaseNodeStorageLease(directory: string, lockToken: string): boolean;
