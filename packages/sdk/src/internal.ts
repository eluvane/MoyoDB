export function isRecord(value: unknown): value is Record<string, unknown> {
    return value !== null && typeof value === 'object';
}

export function compareStringsByCodeUnit(left: string, right: string): number {
    if (left < right) {
        return -1;
    }
    return left > right ? 1 : 0;
}

export function withTimeout<T>(promise: Promise<T>, timeoutMs: number, fallback: T): Promise<T> {
    return new Promise((resolve) => {
        const timeout = setTimeout(() => resolve(fallback), timeoutMs);
        promise.then(
            (value) => {
                clearTimeout(timeout);
                resolve(value);
            },
            () => {
                clearTimeout(timeout);
                resolve(fallback);
            }
        );
    });
}
