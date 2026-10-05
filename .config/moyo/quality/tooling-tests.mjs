import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
    closeSync,
    copyFileSync,
    mkdirSync,
    mkdtempSync,
    openSync,
    readFileSync,
    rmSync,
    writeFileSync
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const temporaryRoot = join(root, '.tmp', 'quality-tooling-tests');

function fixture(t, scripts) {
    mkdirSync(temporaryRoot, { recursive: true });
    const directory = mkdtempSync(join(temporaryRoot, 'case-'));
    t.after(() => {
        assert.equal(dirname(directory), temporaryRoot);
        rmSync(directory, { recursive: true, force: true });
    });
    for (const script of scripts) {
        const destination = join(directory, script);
        mkdirSync(dirname(destination), { recursive: true });
        copyFileSync(join(root, script), destination);
    }
    return directory;
}

function write(directory, file, value) {
    const destination = join(directory, file);
    mkdirSync(dirname(destination), { recursive: true });
    writeFileSync(destination, typeof value === 'string' ? value : JSON.stringify(value));
}

function run(directory, script, args = []) {
    const logPath = join(directory, 'command.log');
    const descriptor = openSync(logPath, 'w');
    let result;
    try {
        result = spawnSync(process.execPath, [resolve(directory, script), ...args], {
            cwd: directory,
            stdio: ['ignore', descriptor, descriptor],
            timeout: 10_000,
            shell: false
        });
    } finally {
        closeSync(descriptor);
    }
    assert.ifError(result.error);
    return { status: result.status, output: readFileSync(logPath, 'utf8') };
}

const workflowScript = '.config/moyo/quality/check-workflows.mjs';
const packageLintScript = '.config/moyo/quality/check-package-json.mjs';
const markdownLintScript = '.config/moyo/quality/check-markdown.mjs';
const lintCollector = '.config/moyo/quality/collect-lint-files.mjs';

function packageLintFixture(t) {
    const directory = fixture(t, [packageLintScript, lintCollector]);
    copyFileSync(join(root, '.config/moyo/lints/npm-package-json-lint.json'), join(directory, 'rules.json'));
    write(
        directory,
        '.config/moyo/lints/npm-package-json-lint.json',
        readFileSync(join(directory, 'rules.json'), 'utf8')
    );
    write(directory, '.config/moyo/formatters/prettierignore', 'node_modules/\n.tmp/\n');
    write(directory, 'package.json', {
        name: 'lint-fixture',
        version: '1.0.0',
        description: 'Package lint fixture',
        license: 'SEE LICENSE IN LICENSE',
        scripts: {},
        repository: {},
        keywords: [],
        engines: {}
    });
    return directory;
}

test('package lint checks nested manifests while excluding dependency and temporary files', (t) => {
    const directory = packageLintFixture(t);
    write(directory, 'packages/child/package.json', readFileSync(join(directory, 'package.json'), 'utf8'));
    write(directory, 'node_modules/bad/package.json', 'invalid');
    write(directory, '.tmp/bad/package.json', 'invalid');
    const result = run(directory, packageLintScript);
    assert.equal(result.status, 0, result.output);
    assert.match(result.output, /2 files checked/u);
});

test('package lint preserves required-field errors and nonblocking warnings', (t) => {
    const directory = packageLintFixture(t);
    const manifest = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
    delete manifest.repository;
    write(directory, 'package.json', manifest);
    const warning = run(directory, packageLintScript);
    assert.equal(warning.status, 0, warning.output);
    assert.match(warning.output, /warning require-repository/u);
    delete manifest.name;
    write(directory, 'package.json', manifest);
    const missing = run(directory, packageLintScript);
    assert.equal(missing.status, 1, missing.output);
    assert.match(missing.output, /error require-name/u);
});

test('package lint rejects duplicate JSON properties before parsing can hide them', (t) => {
    const directory = packageLintFixture(t);
    const original = readFileSync(join(directory, 'package.json'), 'utf8');
    write(directory, 'package.json', original.replace('"scripts":{}', '"scripts":{"a":1,"a":2}'));
    const result = run(directory, packageLintScript);
    assert.equal(result.status, 1, result.output);
    assert.match(result.output, /no-duplicate-properties.*a/u);
});

test('package lint rejects incompatible licenses, unsorted dependencies and invalid JSON', (t) => {
    const directory = packageLintFixture(t);
    const manifest = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'));
    manifest.license = 'UNLICENSED';
    manifest.dependencies = { z: '1.0.0', a: '1.0.0' };
    manifest.devDependencies = { z: '1.0.0', a: '1.0.0' };
    write(directory, 'package.json', manifest);
    const policy = run(directory, packageLintScript);
    assert.equal(policy.status, 1, policy.output);
    assert.match(policy.output, /valid-values-license/u);
    assert.match(policy.output, /prefer-alphabetical-dependencies/u);
    assert.match(policy.output, /prefer-alphabetical-devDependencies/u);
    write(directory, 'packages/invalid/package.json', '{');
    const malformed = run(directory, packageLintScript);
    assert.equal(malformed.status, 1, malformed.output);
    assert.match(malformed.output, /valid-json/u);
});

test('package lint fails on unsupported policy rules instead of skipping them', (t) => {
    const directory = packageLintFixture(t);
    write(directory, '.config/moyo/lints/npm-package-json-lint.json', { rules: { 'unknown-policy': 'error' } });
    const result = run(directory, packageLintScript);
    assert.notEqual(result.status, 0, result.output);
    assert.match(result.output, /Unsupported package lint rule/u);
});

test('Markdown lint checks hidden documentation and preserves configured exclusions', (t) => {
    const directory = fixture(t, [markdownLintScript, lintCollector]);
    write(directory, '.config/moyo/lints/markdownlint-cli2.jsonc', {
        config: { default: false, MD018: true },
        globs: ['**/*.md', '!.tmp/**', '!node_modules/**']
    });
    write(directory, 'README.md', '# Heading\n');
    write(directory, '.tmp/bad.md', '#Bad heading\n');
    write(directory, 'node_modules/bad/README.md', '#Bad heading\n');
    const valid = run(directory, markdownLintScript);
    assert.equal(valid.status, 0, valid.output);
    write(directory, '.config/README.md', '#Bad heading\n');
    const hidden = run(directory, markdownLintScript);
    assert.equal(hidden.status, 1, hidden.output);
    assert.match(hidden.output, /MD018/u);
});

function workflow(action) {
    return `permissions: {}\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: ${action}\n`;
}

test('workflow policy rejects moving action refs in unnamed steps', (t) => {
    const directory = fixture(t, [workflowScript]);
    write(directory, '.github/workflows/witness.yml', workflow('actions/setup-node@main'));
    const result = run(directory, workflowScript);
    assert.equal(result.status, 1, result.output);
    assert.match(result.output, /must not use a moving ref/u);
});

test('workflow policy rejects unpinned actions in unnamed steps', (t) => {
    const directory = fixture(t, [workflowScript]);
    write(directory, '.github/workflows/witness.yml', workflow('actions/setup-node'));
    const result = run(directory, workflowScript);
    assert.equal(result.status, 1, result.output);
    assert.match(result.output, /must be pinned/u);
});

test('workflow policy accepts a version pin in an unnamed step', (t) => {
    const directory = fixture(t, [workflowScript]);
    write(directory, '.github/workflows/witness.yml', workflow('actions/setup-node@v7'));
    const result = run(directory, workflowScript);
    assert.equal(result.status, 0, result.output);
});

test('workflow policy rejects a quoted moving action ref', (t) => {
    const directory = fixture(t, [workflowScript]);
    write(directory, '.github/workflows/witness.yml', workflow('"actions/setup-node@main"'));
    const result = run(directory, workflowScript);
    assert.equal(result.status, 1, result.output);
    assert.match(result.output, /must not use a moving ref/u);
});

test('workflow policy checks npm install flags in unnamed run steps', (t) => {
    const directory = fixture(t, [workflowScript]);
    write(
        directory,
        '.github/workflows/witness.yml',
        'permissions: {}\njobs:\n  check:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: npm ci\n'
    );
    const result = run(directory, workflowScript);
    assert.equal(result.status, 1, result.output);
    assert.match(result.output, /npm ci must include --ignore-scripts/u);
});

const jsonScript = '.config/moyo/quality/check-json-files.mjs';
for (const [name, source] of [
    ['adjacent number tokens separated by an empty comment', '{"value": 1/**/2}'],
    ['an unterminated final block comment', '{"value": 1} /*']
]) {
    test(`JSONC policy rejects ${name}`, (t) => {
        const directory = fixture(t, [jsonScript]);
        write(directory, 'witness.jsonc', source);
        const result = run(directory, jsonScript);
        assert.equal(result.status, 1, result.output);
        assert.match(result.output, /Invalid JSON\/JSONC/u);
    });
}

test('JSONC policy accepts comments while preserving comment-like string contents', (t) => {
    const directory = fixture(t, [jsonScript]);
    write(directory, 'witness.jsonc', '// header\n{"url": "https://example.test/a/*b*/", "value": /* comment */ 1}\n');
    const result = run(directory, jsonScript);
    assert.equal(result.status, 0, result.output);
});

const benchmarkScripts = [
    'scripts/benchmark/collect.mjs',
    'scripts/benchmark/compare.mjs',
    'scripts/benchmark/reporting.mjs'
];
function report(persistentContext, mean = 10, profile = 'full') {
    return {
        schemaVersion: 1,
        profile,
        browser: { name: 'chromium', version: '123', platform: 'test' },
        environment: {
            persistentContext,
            indexedDbDurability: 'strict',
            moyoDbDurability: 'strict',
            sdkBuildMode: 'production',
            wasmBuildMode: 'release',
            backendPath: 'opfs',
            headless: true
        },
        results: [
            {
                status: 'ok',
                engine: 'moyodb',
                workloadName: 'put',
                recordCount: 100,
                keySize: 16,
                valueSize: 32,
                batchSize: 10,
                transactionBoundaries: 'one batch per transaction',
                warmupCount: 1,
                sampleCount: 3,
                stats: { mean, p50: mean, p95: mean, p99: mean }
            }
        ]
    };
}

test('benchmark collection preserves settings used to decide comparability', (t) => {
    const directory = fixture(t, benchmarkScripts);
    write(directory, 'packages/sdk/bench/results/incognito.json', report(false));
    write(directory, 'packages/sdk/bench/results/persistent.json', report(true));
    const collected = run(directory, benchmarkScripts[0], ['--from-results', '--out-dir', 'baseline']);
    assert.equal(collected.status, 0, collected.output);
    const baseline = JSON.parse(readFileSync(join(directory, 'baseline/baseline.json'), 'utf8'));
    const cases = baseline.suites[0].cases;
    assert.equal(cases.length, 2);
    assert.equal(cases[0].comparisonContext.profile, 'full');
    assert.equal(cases[0].comparisonContext.persistentContext, false);
    assert.equal(cases[1].comparisonContext.persistentContext, true);
    assert.equal(cases[0].comparisonContext.sampleCount, 3);
});

test('benchmark comparison retains both context modes and compares only matching settings', (t) => {
    const directory = fixture(t, benchmarkScripts);
    write(directory, 'packages/sdk/bench/results/incognito.json', report(false));
    write(directory, 'packages/sdk/bench/results/persistent.json', report(true, 20));
    assert.equal(run(directory, benchmarkScripts[0], ['--from-results', '--out-dir', 'previous']).status, 0);
    write(directory, 'packages/sdk/bench/results/persistent.json', report(true, 40));
    assert.equal(run(directory, benchmarkScripts[0], ['--from-results', '--out-dir', 'current']).status, 0);
    const result = run(directory, benchmarkScripts[1], [
        '--previous',
        'previous/baseline.json',
        '--current',
        'current/baseline.json',
        '--out-dir',
        'comparison'
    ]);
    assert.equal(result.status, 0, result.output);
    const comparison = JSON.parse(readFileSync(join(directory, 'comparison/comparison.json'), 'utf8'));
    assert.equal(comparison.comparisons.length, 2);
    assert.equal(comparison.comparisons.filter((entry) => entry.regression).length, 1);
    const gate = run(directory, benchmarkScripts[1], [
        '--previous',
        'previous/baseline.json',
        '--current',
        'current/baseline.json',
        '--out-dir',
        'comparison',
        '--fail-on-regression'
    ]);
    assert.equal(gate.status, 1, gate.output);
    assert.match(gate.output, /failed with 1 regression/u);
});

test('benchmark gate accepts unchanged metrics with complete matching settings', (t) => {
    const directory = fixture(t, benchmarkScripts);
    write(directory, 'packages/sdk/bench/results/browser.json', report(false));
    assert.equal(run(directory, benchmarkScripts[0], ['--from-results', '--out-dir', 'previous']).status, 0);
    assert.equal(run(directory, benchmarkScripts[0], ['--from-results', '--out-dir', 'current']).status, 0);
    const result = run(directory, benchmarkScripts[1], [
        '--previous',
        'previous/baseline.json',
        '--current',
        'current/baseline.json',
        '--out-dir',
        'comparison',
        '--fail-on-regression'
    ]);
    assert.equal(result.status, 0, result.output);
    const comparison = JSON.parse(readFileSync(join(directory, 'comparison/comparison.json'), 'utf8'));
    assert.equal(comparison.comparisons.length, 1);
    assert.equal(comparison.comparisons[0].regression, false);
});

for (const [name, change] of [
    [
        'profile',
        (value) => {
            value.profile = 'smoke';
        }
    ],
    [
        'context mode',
        (value) => {
            value.environment.persistentContext = true;
        }
    ],
    [
        'browser version',
        (value) => {
            value.browser.version = '124';
        }
    ],
    [
        'durability',
        (value) => {
            value.environment.indexedDbDurability = 'relaxed';
        }
    ],
    [
        'sample count',
        (value) => {
            value.results[0].sampleCount = 1;
        }
    ]
]) {
    test(`benchmark gate reports incompatible ${name} without computing a regression`, (t) => {
        const directory = fixture(t, benchmarkScripts);
        write(directory, 'packages/sdk/bench/results/browser.json', report(false));
        assert.equal(run(directory, benchmarkScripts[0], ['--from-results', '--out-dir', 'previous']).status, 0);
        const current = report(false, 40);
        change(current);
        write(directory, 'packages/sdk/bench/results/browser.json', current);
        assert.equal(run(directory, benchmarkScripts[0], ['--from-results', '--out-dir', 'current']).status, 0);
        const result = run(directory, benchmarkScripts[1], [
            '--previous',
            'previous/baseline.json',
            '--current',
            'current/baseline.json',
            '--out-dir',
            'comparison',
            '--fail-on-regression'
        ]);
        assert.equal(result.status, 1, result.output);
        assert.match(result.output, /no comparable benchmark cases/u);
        const comparison = JSON.parse(readFileSync(join(directory, 'comparison/comparison.json'), 'utf8'));
        assert.equal(comparison.comparisons.length, 0);
        assert.equal(comparison.unmatchedCases.length, 1);
    });
}

test('benchmark gate retains first-run informational behavior', (t) => {
    const directory = fixture(t, benchmarkScripts);
    write(directory, 'packages/sdk/bench/results/browser.json', report(false));
    assert.equal(run(directory, benchmarkScripts[0], ['--from-results', '--out-dir', 'current']).status, 0);
    const result = run(directory, benchmarkScripts[1], [
        '--current',
        'current/baseline.json',
        '--out-dir',
        'comparison',
        '--fail-on-regression'
    ]);
    assert.equal(result.status, 0, result.output);
    const comparison = JSON.parse(readFileSync(join(directory, 'comparison/comparison.json'), 'utf8'));
    assert.equal(comparison.previousAvailable, false);
});

test('benchmark gate refuses to compare legacy rows with unknown settings', (t) => {
    const directory = fixture(t, benchmarkScripts);
    const legacy = {
        suites: [
            {
                suite: 'browser-sdk-wasm-opfs',
                cases: [
                    {
                        id: 'moyodb:put',
                        label: 'put',
                        metrics: { opsPerSec: 100, avgLatencyMs: 10, p95LatencyMs: 10, p99LatencyMs: 10 }
                    }
                ]
            }
        ]
    };
    write(directory, 'previous/baseline.json', legacy);
    write(directory, 'current/baseline.json', legacy);
    const result = run(directory, benchmarkScripts[1], [
        '--previous',
        'previous/baseline.json',
        '--current',
        'current/baseline.json',
        '--out-dir',
        'comparison',
        '--fail-on-regression'
    ]);
    assert.equal(result.status, 1, result.output);
    assert.match(result.output, /no comparable benchmark cases/u);
});
