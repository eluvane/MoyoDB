import { defineConfig, devices } from '@playwright/test';
const isCi = Boolean(
    (
        globalThis as {
            process?: {
                env?: Record<string, string | undefined>;
            };
        }
    ).process?.env?.CI
);
const chromiumExecutablePath = (
    globalThis as {
        process?: {
            env?: Record<string, string | undefined>;
        };
    }
).process?.env?.MOYODB_CHROMIUM_EXECUTABLE_PATH;
const disableVideo =
    (
        globalThis as {
            process?: {
                env?: Record<string, string | undefined>;
            };
        }
    ).process?.env?.MOYODB_DISABLE_VIDEO === '1';
const chromiumUse = chromiumExecutablePath
    ? { ...devices['Desktop Chrome'], launchOptions: { executablePath: chromiumExecutablePath } }
    : { ...devices['Desktop Chrome'] };
export default defineConfig({
    testDir: './tests',
    timeout: 60000,
    workers: isCi ? 1 : undefined,
    outputDir: './test-results',
    reporter: [['list'], ['html', { open: 'never', outputFolder: 'playwright-report' }]],
    expect: {
        timeout: 10000
    },
    use: {
        baseURL: 'http://127.0.0.1:4173',
        trace: 'retain-on-failure',
        video: disableVideo ? 'off' : 'retain-on-failure',
        screenshot: 'only-on-failure'
    },
    webServer: {
        command: 'npm run dev:test',
        url: 'http://127.0.0.1:4173',
        timeout: 120000,
        reuseExistingServer: !isCi
    },
    projects: [
        { name: 'chromium', use: chromiumUse },
        { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
        { name: 'webkit', use: { ...devices['Desktop Safari'] } }
    ]
});
