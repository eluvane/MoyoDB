#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { closeSync, cpSync, mkdirSync, mkdtempSync, openSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const proofRoot = join(root, 'proofs', 'moyodb_proofs');
const temporaryRoot = join(root, '.tmp', 'lean-btree-checker');
mkdirSync(temporaryRoot, { recursive: true });
const work = mkdtempSync(join(temporaryRoot, 'pass-'));
const lake = process.env.MOYO_LAKE ?? (process.platform === 'win32' ? 'lake.exe' : 'lake');
const cargo = process.platform === 'win32' ? 'cargo.exe' : 'cargo';
const checker = join(
    proofRoot,
    '.lake',
    'build',
    'bin',
    `check_btree_pages${process.platform === 'win32' ? '.exe' : ''}`
);
const environment = { ...process.env, CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? '4' };

function run(command, args, cwd, env = environment) {
    const result = spawnSync(command, args, {
        cwd,
        env,
        stdio: 'inherit',
        timeout: 180_000,
        shell: false
    });
    if (result.error || result.status !== 0) {
        throw new Error(`${command} failed: ${result.error?.message ?? result.status}`);
    }
}

function check(path, expectedError) {
    const logPath = `${path}.log`;
    const log = openSync(logPath, 'w');
    let result;
    try {
        result = spawnSync(lake, ['env', checker, path], {
            cwd: proofRoot,
            env: environment,
            stdio: ['inherit', log, log],
            timeout: 60_000,
            shell: false
        });
    } finally {
        closeSync(log);
    }
    if (result.error) throw result.error;
    const diagnostic = readFileSync(logPath, 'utf8');
    if (expectedError) {
        if (result.status !== 1 || !expectedError.test(diagnostic)) {
            throw new Error(`Negative witness did not fail as specified: ${diagnostic}`);
        }
        process.stdout.write(`Rejected: ${diagnostic.trim()}\n`);
    } else {
        if (result.status !== 0 || !/15 frames accepted/u.test(diagnostic)) {
            throw new Error(`Positive conformance failed: ${diagnostic}`);
        }
        process.stdout.write(`${diagnostic.trim()}\n`);
    }
}

function exportCorpus(cwd, path, witness = false, packageName = 'moyodb-engine') {
    run(cargo, ['test', '--locked', '-p', packageName, '--test', 'lean_btree_pages'], cwd, {
        ...environment,
        CARGO_TARGET_DIR: join(root, 'target'),
        MOYO_BTREE_CORPUS_OUT: path,
        MOYO_BTREE_CORPUS_MODE: witness ? 'witness' : 'full'
    });
}

function editPage(frame, id, change) {
    const page = frame.pages.find((candidate) => candidate.id === id);
    if (!page) throw new Error(`Fixture has no page ${id}`);
    const bytes = Buffer.from(page.bytes_hex, 'hex');
    change(bytes);
    page.bytes_hex = bytes.toString('hex');
}

function firstLeaf(frame) {
    const page = frame.pages.find((candidate) => Buffer.from(candidate.bytes_hex, 'hex')[16] === 1);
    if (!page) throw new Error('Fixture must contain a leaf');
    return page.id;
}

run(lake, ['build', '--wfail', 'MoyoDbProofs', 'check_btree_pages'], proofRoot);
const positive = join(work, 'positive.json');
exportCorpus(root, positive);
check(positive);
const corpus = JSON.parse(readFileSync(positive, 'utf8'));
let negatives = 0;

function reject(name, mutate, error) {
    const changed = structuredClone(corpus);
    mutate(changed);
    const path = join(work, `${name}.json`);
    writeFileSync(path, JSON.stringify(changed));
    check(path, error);
    negatives += 1;
}

reject(
    'missing-page',
    (data) => {
        const frame = data.scenarios[0].frames[0];
        frame.pages = frame.pages.filter((page) => page.id !== frame.root);
    },
    /missing reachable page/u
);
reject(
    'duplicate-page',
    (data) => {
        const frame = data.scenarios[0].frames[0];
        frame.pages.push(structuredClone(frame.pages[0]));
    },
    /duplicate raw page ids/u
);
reject(
    'overlap',
    (data) => {
        const frame = data.scenarios[0].frames[0];
        editPage(frame, firstLeaf(frame), (bytes) => bytes.writeUInt16LE(bytes.readUInt16LE(34), 36));
    },
    /overlapping cells/u
);
reject(
    'oversized-cell',
    (data) => {
        const frame = data.scenarios[0].frames[0];
        editPage(frame, firstLeaf(frame), (bytes) => bytes.writeUInt16LE(65535, bytes.readUInt16LE(34)));
    },
    /outside page/u
);
reject(
    'cycle',
    (data) => {
        const frame = data.scenarios[0].frames[0];
        editPage(frame, frame.root, (bytes) => bytes.writeBigUInt64LE(BigInt(frame.root), bytes.readUInt16LE(34) + 4));
    },
    /cycle or repeated reachability/u
);
reject(
    'overflow',
    (data) => {
        const frame = data.scenarios[0].frames[0];
        editPage(frame, firstLeaf(frame), (bytes) => {
            bytes[bytes.readUInt16LE(34) + 2] = 2;
        });
    },
    /unsupported overflow/u
);
reject(
    'cow-write',
    (data) => {
        const previous = data.scenarios[0].frames[0];
        const frame = data.scenarios[0].frames[1];
        editPage(frame, previous.root, (bytes) => {
            bytes[bytes.readUInt16LE(34) + 12] ^= 1;
        });
    },
    /retained frame 0:.*separator\/minimum/u
);
reject(
    'missing-lookups',
    (data) => {
        data.scenarios[0].frames[0].lookups = [];
    },
    /missing mandatory lookup/u
);
reject(
    'missing-scans',
    (data) => {
        data.scenarios[0].frames[0].scans = [];
    },
    /missing scans/u
);
reject(
    'missing-retained-root',
    (data) => {
        data.scenarios[0].frames[1].retained_roots = [];
    },
    /missing retained COW root/u
);
reject(
    'missing-frame',
    (data) => {
        data.scenarios[0].frames.pop();
    },
    /missing mandatory frame/u
);
reject(
    'missing-scenario',
    (data) => {
        data.scenarios.pop();
    },
    /missing mandatory scenario/u
);

const mutations = [
    {
        name: 'separator',
        before: 'separator: group[0].key.clone(),',
        after: 'separator: group[group.len() - 1].key.clone(),',
        error: /separator\/minimum mismatch/u
    },
    {
        name: 'routing',
        before: 'if compare_keys(cell.separator, key) != Ordering::Greater {',
        after: 'if compare_keys(cell.separator, key) == Ordering::Less {',
        error: /lookup mismatch/u
    }
];

for (const mutation of mutations) {
    // Mutate temporary copies so the source tree stays intact.
    const copy = join(work, `mutant-${mutation.name}`);
    mkdirSync(copy, { recursive: true });
    for (const file of ['Cargo.toml', 'Cargo.lock', 'LICENSE', 'README.md']) {
        cpSync(join(root, file), join(copy, file));
    }
    mkdirSync(join(copy, 'crates'), { recursive: true });
    cpSync(join(root, 'crates', 'engine'), join(copy, 'crates', 'engine'), { recursive: true });
    // Distinct package and library names prevent cdylib cache collisions.
    const packageName = `moyodb-engine-checker-${mutation.name}`;
    const libraryName = `moyodb_engine_checker_${mutation.name}`;
    const manifestPath = join(copy, 'crates', 'engine', 'Cargo.toml');
    const manifest = readFileSync(manifestPath, 'utf8');
    writeFileSync(
        manifestPath,
        manifest
            .replace('name = "moyodb-engine"', `name = "${packageName}"`)
            .replace('name = "moyodb_engine"', `name = "${libraryName}"`)
    );
    const lockPath = join(copy, 'Cargo.lock');
    writeFileSync(
        lockPath,
        readFileSync(lockPath, 'utf8').replace('name = "moyodb-engine"', `name = "${packageName}"`)
    );
    const exporterPath = join(copy, 'crates', 'engine', 'tests', 'lean_btree_pages.rs');
    writeFileSync(exporterPath, `use ${libraryName} as moyodb_engine;\n${readFileSync(exporterPath, 'utf8')}`);
    const sourcePath = join(copy, 'crates', 'engine', 'src', 'btree.rs');
    const source = readFileSync(sourcePath, 'utf8');
    if (source.split(mutation.before).length !== 2) {
        throw new Error(`Production witness ${mutation.name} must match exactly one site`);
    }
    writeFileSync(sourcePath, source.replace(mutation.before, mutation.after));
    const path = resolve(copy, 'corpus.json');
    exportCorpus(copy, path, true, packageName);
    check(path, mutation.error);
    negatives += 1;
}

// Recheck the original package for cache contamination from the mutants.
const restored = join(work, 'original-after-mutants.json');
exportCorpus(root, restored);
check(restored);

process.stdout.write(
    `Lean B-tree gate passed: 15 positive frames, ${negatives} negative witnesses (2 production mutants). Artifacts: ${work}\n`
);
