import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import { test } from 'node:test';
import {
    WASM_PACK_VERSION,
    installWasmPack,
    installedWasmPackMatches,
    selectWasmPackTarget
} from './install-wasm-pack.mjs';

const RELEASE = 'https://github.com/wasm-bindgen/wasm-pack/releases/download/v0.15.0';

test('selects the pinned linux musl archive', () => {
    const asset = selectWasmPackTarget('linux', 'x64');
    assert.equal(WASM_PACK_VERSION, '0.15.0');
    assert.equal(asset.target, 'x86_64-unknown-linux-musl');
    assert.equal(asset.binaryName, 'wasm-pack');
    assert.equal(asset.archiveName, 'wasm-pack-v0.15.0-x86_64-unknown-linux-musl.tar.gz');
    assert.equal(asset.url, `${RELEASE}/${asset.archiveName}`);
});

test('selects the pinned windows archive with wasm-pack.exe', () => {
    const asset = selectWasmPackTarget('win32', 'x64');
    assert.equal(asset.target, 'x86_64-pc-windows-msvc');
    assert.equal(asset.binaryName, 'wasm-pack.exe');
    assert.equal(asset.archiveName, 'wasm-pack-v0.15.0-x86_64-pc-windows-msvc.tar.gz');
    assert.equal(asset.url, `${RELEASE}/${asset.archiveName}`);
});

test('rejects targets that have no pinned archive', () => {
    assert.throws(() => selectWasmPackTarget('darwin', 'arm64'), /darwin\/arm64/u);
    assert.throws(() => selectWasmPackTarget('linux', 'arm64'), /linux\/arm64/u);
});

test('accepts only wasm-pack 0.15.0 version output', () => {
    assert.equal(installedWasmPackMatches('wasm-pack 0.15.0'), true);
    assert.equal(installedWasmPackMatches('wasm-pack 0.15.0\r\n'), true);
    assert.equal(installedWasmPackMatches('wasm-pack 0.14.0'), false);
    assert.equal(installedWasmPackMatches('wasm-pack 0.15.1'), false);
    assert.equal(installedWasmPackMatches('wasm-pack 0.15.0-dev'), false);
    assert.equal(installedWasmPackMatches('wasm-pack 0.15.00'), false);
    assert.equal(installedWasmPackMatches('wasm-pack 10.15.0'), false);
    assert.equal(installedWasmPackMatches(''), false);
    assert.equal(installedWasmPackMatches(undefined), false);
});

test('skips the download when wasm-pack 0.15.0 is already on PATH', async () => {
    let fetched = false;
    const result = await installWasmPack({
        readInstalledVersion: () => 'wasm-pack 0.15.0',
        fetchImpl: async () => {
            fetched = true;
            throw new Error('download should not run');
        }
    });
    assert.equal(result.skipped, true);
    assert.equal(fetched, false);
});

test('fails closed when the archive response is not HTTP 200', async () => {
    let requested = '';
    await assert.rejects(
        () =>
            installWasmPack({
                platform: 'linux',
                arch: 'x64',
                env: {},
                readInstalledVersion: () => '',
                fetchImpl: async (url) => {
                    requested = url;
                    return { status: 404, body: null };
                }
            }),
        /HTTP 404/u
    );
    assert.equal(requested, `${RELEASE}/wasm-pack-v0.15.0-x86_64-unknown-linux-musl.tar.gz`);
});

test('requests the windows archive when a different version is installed', async () => {
    let requested = '';
    await assert.rejects(
        () =>
            installWasmPack({
                platform: 'win32',
                arch: 'x64',
                env: {},
                readInstalledVersion: () => 'wasm-pack 0.13.1',
                fetchImpl: async (url) => {
                    requested = url;
                    return { status: 503, body: null };
                }
            }),
        /HTTP 503/u
    );
    assert.equal(requested, `${RELEASE}/wasm-pack-v0.15.0-x86_64-pc-windows-msvc.tar.gz`);
});

function localArchive(directoryName, binaryName, contents) {
    const root = mkdtempSync(join(tmpdir(), 'wasm-pack-fixture-'));
    mkdirSync(join(root, directoryName));
    writeFileSync(join(root, directoryName, binaryName), contents);
    const packed = spawnSync('tar', ['-czf', 'archive.tar.gz', directoryName], {
        cwd: root,
        shell: false,
        windowsHide: true
    });
    assert.equal(packed.status, 0, packed.stderr?.toString() || 'tar could not build the local fixture');
    return {
        root,
        bytes: readFileSync(join(root, 'archive.tar.gz'))
    };
}

test('extracts a local archive onto GITHUB_PATH without downloading', async (t) => {
    const asset = selectWasmPackTarget('win32', 'x64');
    const fixture = localArchive(asset.archiveName.replace(/\.tar\.gz$/u, ''), asset.binaryName, 'wasm-pack-binary');
    const destDir = join(fixture.root, 'dest');
    const githubPath = join(fixture.root, 'github-path.txt');
    t.after(() => {
        rmSync(fixture.root, { recursive: true, force: true });
    });
    const result = await installWasmPack({
        platform: 'win32',
        arch: 'x64',
        env: { GITHUB_PATH: githubPath },
        destDir,
        readInstalledVersion: () => '',
        fetchImpl: async (url) => {
            assert.equal(url, asset.url);
            return {
                status: 200,
                body: Readable.toWeb(Readable.from(fixture.bytes))
            };
        }
    });
    assert.equal(result.skipped, false);
    assert.equal(readFileSync(result.binaryPath, 'utf8'), 'wasm-pack-binary');
    assert.equal(readFileSync(githubPath, 'utf8').trim(), result.binDir);
});
