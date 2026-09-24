import { execFileSync, spawn } from 'node:child_process';
import { createRequire } from 'node:module';

const profiles = {
    'bench:browser': {
        env: { MOYODB_RUN_BENCH: '1', MOYODB_BENCH_ENGINE: 'all', MOYODB_DISABLE_VIDEO: '1' },
        args: ['test', 'tests/browser-bench.spec.ts', '--project=chromium']
    },
    'bench:indexeddb': {
        env: { MOYODB_RUN_BENCH: '1', MOYODB_BENCH_ENGINE: 'indexeddb', MOYODB_DISABLE_VIDEO: '1' },
        args: ['test', 'tests/browser-bench.spec.ts', '--project=chromium']
    },
    'bench:opfs': {
        env: { MOYODB_RUN_BENCH: '1', MOYODB_BENCH_ENGINE: 'moyodb', MOYODB_DISABLE_VIDEO: '1' },
        args: ['test', 'tests/browser-bench.spec.ts', '--project=chromium']
    },
    'test:soak': {
        env: { MOYODB_RUN_SOAK: '1' },
        args: ['test', 'tests/soak.spec.ts', '--project=chromium']
    }
};

const profileName = process.argv[2];
const profile = profiles[profileName];

if (!profile) {
    console.error(`Unknown Playwright profile: ${profileName ?? '<missing>'}`);
    process.exit(1);
}

function gitRevision() {
    try {
        const git = (args) =>
            execFileSync('git', args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
        const sha = git(['rev-parse', 'HEAD']);
        return git(['status', '--porcelain', '--untracked-files=no']) ? `${sha}-dirty` : sha;
    } catch {
        return 'unknown';
    }
}

const env = { ...process.env, ...profile.env };
if (profile.env.MOYODB_RUN_BENCH === '1' && !env.MOYODB_BENCH_GIT_SHA) {
    env.MOYODB_BENCH_GIT_SHA = env.GITHUB_SHA ?? gitRevision();
}

const require = createRequire(import.meta.url);
const playwrightCli = require.resolve('@playwright/test/cli');
const child = spawn(process.execPath, [playwrightCli, ...profile.args], {
    env,
    stdio: 'inherit'
});

child.on('exit', (code, signal) => {
    if (signal) {
        console.error(`Playwright exited from signal ${signal}`);
        process.exit(1);
    }

    process.exit(code ?? 1);
});

child.on('error', (error) => {
    console.error(error.message);
    process.exit(1);
});
