export function isRecord(value: unknown): value is Record<string, unknown> {
    return value !== null && typeof value === 'object';
}

export function compareStringsByCodeUnit(left: string, right: string): number {
    if (left < right) {
        return -1;
    }
    return left > right ? 1 : 0;
}

/**
 * Delays above 2^31-1 do not fit in the platform timer word. Node fires them
 * after 1ms; other runtimes can wrap them to zero. Clamp instead of aborting.
 */
export const MAX_TIMER_DELAY_MS = 2_147_483_647;

export function clampTimerDelayMs(delayMs: number): number {
    if (!Number.isFinite(delayMs) || delayMs <= 0) {
        return 0;
    }
    return delayMs > MAX_TIMER_DELAY_MS ? MAX_TIMER_DELAY_MS : delayMs;
}

export function withTimeout<T>(promise: Promise<T>, timeoutMs: number, fallback: T): Promise<T> {
    return new Promise((resolve) => {
        const timeout = setTimeout(() => resolve(fallback), clampTimerDelayMs(timeoutMs));
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
