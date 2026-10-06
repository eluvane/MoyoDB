import { defineConfig, devices } from '@playwright/test';
const env = (
    globalThis as {
        process?: {
            env?: Record<string, string | undefined>;
        };
    }
).process?.env;
const isCi = Boolean(env?.CI);
const chromiumExecutablePath = env?.MOYODB_CHROMIUM_EXECUTABLE_PATH;
const disableVideo = env?.MOYODB_DISABLE_VIDEO === '1';
const chromiumUse = chromiumExecutablePath
    ? { ...devices['Desktop Chrome'], launchOptions: { executablePath: chromiumExecutablePath } }
    : { ...devices['Desktop Chrome'] };
const outputProfile = env?.MOYODB_PLAYWRIGHT_PROFILE ?? 'sdk';
const wasmReady = env?.MOYODB_WASM_READY === '1';
const projectsByName = {
    chromium: { name: 'chromium', use: chromiumUse },
    firefox: { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
    webkit: { name: 'webkit', use: { ...devices['Desktop Safari'] } }
};
const requestedProjectNames = (env?.MOYODB_PLAYWRIGHT_PROJECTS ?? '')
    .split(',')
    .map((name) => name.trim())
    .filter((name) => name.length > 0);
const projects =
    requestedProjectNames.length === 0
        ? [projectsByName.chromium, projectsByName.firefox, projectsByName.webkit]
        : requestedProjectNames.flatMap((name) => {
              if (name === 'chromium' || name === 'firefox' || name === 'webkit') {
                  return [projectsByName[name]];
              }
              return [];
          });
export default defineConfig({
    testDir: './tests',
    testIgnore: '**/*.test.mjs',
    timeout: 60000,
    workers: isCi ? 2 : undefined,
    outputDir: `./test-results/${outputProfile}`,
    reporter: [['list'], ['html', { open: 'never', outputFolder: `playwright-report/${outputProfile}` }]],
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
        command: wasmReady ? 'npm run dev:test:serve' : 'npm run dev:test',
        url: 'http://127.0.0.1:4173',
        timeout: 120000,
        reuseExistingServer: !isCi
    },
    projects
});
