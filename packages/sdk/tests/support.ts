import { test, type Browser, type BrowserContext, type BrowserType, type Page } from '@playwright/test';

export interface PersistentContextFactory {
    readonly userDataDir: string;
    launch(): Promise<BrowserContext>;
}

export const persistentContextTest = test.extend<{
    persistentContextFactory: PersistentContextFactory;
}>({
    persistentContextFactory: async ({ browserName, playwright, launchOptions, headless, channel }, use, testInfo) => {
        const userDataDir = testInfo.outputPath(`persistent-user-data-${browserName}`);
        const contexts = new Set<BrowserContext>();
        const baseURL =
            typeof (testInfo.project.use as { baseURL?: unknown }).baseURL === 'string'
                ? (testInfo.project.use as { baseURL: string }).baseURL
                : 'http://127.0.0.1:4173';
        const browserType: BrowserType =
            browserName === 'firefox'
                ? playwright.firefox
                : browserName === 'webkit'
                  ? playwright.webkit
                  : playwright.chromium;
        const factory: PersistentContextFactory = {
            userDataDir,
            async launch() {
                const context = await browserType.launchPersistentContext(userDataDir, {
                    ...launchOptions,
                    headless,
                    channel,
                    baseURL
                });
                contexts.add(context);
                context.on('close', () => contexts.delete(context));
                return context;
            }
        };
        try {
            await use(factory);
        } finally {
            await Promise.all(Array.from(contexts, (context) => context.close().catch(() => undefined)));
        }
    }
});

export async function requireMoyoDbCapabilities(page: Page) {
    // Sync access handles are exclusive. Parallel pages in one profile must not share a file,
    // or a lock error is reported as a missing browser primitive and the test skips.
    const probeFile = `${uniqueDbName('moyodb-support')}.bin`;
    const capabilities = await page.evaluate(async (probeFile) => {
        async function hasSyncAccessHandle(): Promise<boolean> {
            try {
                const source = `
self.onmessage = async (event) => {
  const fileName = String(event.data);
  try {
    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle('__moyodb_support__', { create: true });
    const file = await dir.getFileHandle(fileName, { create: true });
    if (typeof file.createSyncAccessHandle !== 'function') {
      self.postMessage(false);
      return;
    }
    const handle = await file.createSyncAccessHandle();
    handle.close();
    self.postMessage(true);
  } catch {
    self.postMessage(false);
  }
};`;
                const url = URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
                const worker = new Worker(url, { type: 'module' });
                try {
                    return await new Promise<boolean>((resolve) => {
                        const timeout = setTimeout(() => resolve(false), 3000);
                        worker.onmessage = (event) => {
                            clearTimeout(timeout);
                            resolve(event.data === true);
                        };
                        worker.onerror = () => {
                            clearTimeout(timeout);
                            resolve(false);
                        };
                        worker.postMessage(probeFile);
                    });
                } finally {
                    worker.terminate();
                    URL.revokeObjectURL(url);
                }
            } catch {
                return false;
            }
        }

        const result = {
            secure: globalThis.isSecureContext,
            storage: Boolean(navigator.storage),
            getDirectory: typeof navigator.storage.getDirectory === 'function',
            locks: Boolean(navigator.locks),
            broadcast: typeof BroadcastChannel !== 'undefined',
            syncHandle: false
        };
        if (result.getDirectory) {
            result.syncHandle = await hasSyncAccessHandle();
            try {
                const root = await navigator.storage.getDirectory();
                const dir = await root.getDirectoryHandle('__moyodb_support__');
                await dir.removeEntry(probeFile);
            } catch {
                // A leftover unique file must not change the capability result.
            }
        }
        return result;
    }, probeFile);
    const missing = Object.entries(capabilities)
        .filter(([, present]) => !present)
        .map(([name]) => name);
    test.skip(missing.length > 0, `browser lacks required MoyoDB primitives: ${missing.join(', ')}`);
}

export function uniqueDbName(prefix: string): string {
    return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}
export async function prepareMoyoDbPage(page: Page): Promise<void> {
    await page.goto('/');
    await page.waitForFunction(() => typeof window.moyodb.openDB === 'function');
    await requireMoyoDbCapabilities(page);
}

export async function openPairedPages(browser: Browser): Promise<{
    context: BrowserContext;
    first: Page;
    second: Page;
}> {
    const context = await browser.newContext();
    const first = await context.newPage();
    const second = await context.newPage();
    await first.goto('/');
    await second.goto('/');
    await requireMoyoDbCapabilities(first);
    return { context, first, second };
}

export async function requireCompressionStreams(
    page: Page,
    reason = 'browser lacks CompressionStream/DecompressionStream'
): Promise<void> {
    const supported = await page.evaluate(
        () => typeof CompressionStream === 'function' && typeof DecompressionStream === 'function'
    );
    test.skip(!supported, reason);
}
