#!/usr/bin/env node
import { spawn, spawnSync } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { access, appendFile, chmod, copyFile, mkdir, readFile, rm, stat } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { fileURLToPath } from 'node:url';

export const WASM_PACK_VERSION = '0.15.0';

const LINUX_TARGET = 'x86_64-unknown-linux-musl';
const WINDOWS_TARGET = 'x86_64-pc-windows-msvc';
const RELEASE_BASE = `https://github.com/wasm-bindgen/wasm-pack/releases/download/v${WASM_PACK_VERSION}`;
const VERSION_PATTERN = /(?:^|\s)v?0\.15\.0(?:\s|$)/u;

export function selectWasmPackTarget(platform, arch) {
    let target;
    let binaryName;
    if (platform === 'linux' && arch === 'x64') {
        target = LINUX_TARGET;
        binaryName = 'wasm-pack';
    } else if (platform === 'win32' && arch === 'x64') {
        target = WINDOWS_TARGET;
        binaryName = 'wasm-pack.exe';
    } else {
        throw new Error(`No wasm-pack ${WASM_PACK_VERSION} archive for ${platform}/${arch}`);
    }
    const archiveName = `wasm-pack-v${WASM_PACK_VERSION}-${target}.tar.gz`;
    return {
        archiveName,
        binaryName,
        target,
        url: `${RELEASE_BASE}/${archiveName}`
    };
}

export function installedWasmPackMatches(versionOutput) {
    if (typeof versionOutput !== 'string') {
        return false;
    }
    return VERSION_PATTERN.test(versionOutput.trim());
}

function readWasmPackVersion() {
    const result = spawnSync('wasm-pack', ['--version'], {
        encoding: 'utf8',
        shell: false,
        timeout: 15_000,
        windowsHide: true
    });
    if (result.error || result.status !== 0) {
        return '';
    }
    return `${result.stdout ?? ''}${result.stderr ?? ''}`;
}

function extractTarGz(archivePath, destination) {
    const cwd = path.dirname(archivePath);
    const relativeDestination = path.relative(cwd, destination);
    if (relativeDestination === '' || relativeDestination.startsWith('..') || path.isAbsolute(relativeDestination)) {
        throw new Error(`refusing to extract wasm-pack outside ${cwd}`);
    }
    const destinationArg = relativeDestination.split(path.sep).join('/');
    return new Promise((resolve, reject) => {
        let settled = false;
        const finish = (error) => {
            if (settled) {
                return;
            }
            settled = true;
            if (error) {
                reject(error);
                return;
            }
            resolve();
        };
        const child = spawn('tar', ['-xzf', path.basename(archivePath), '-C', destinationArg], {
            cwd,
            shell: false,
            stdio: ['ignore', 'ignore', 'pipe'],
            windowsHide: true
        });
        let stderr = '';
        child.stderr.on('data', (chunk) => {
            stderr += String(chunk);
        });
        child.once('error', (error) => {
            if (error && error.code === 'ENOENT') {
                finish(new Error('tar is required to extract wasm-pack, but it was not found on PATH'));
                return;
            }
            finish(error);
        });
        child.once('close', (code) => {
            if (code === 0) {
                finish();
                return;
            }
            const detail = stderr.trim();
            finish(new Error(`tar failed with status ${code ?? 'unknown'}${detail ? `: ${detail}` : ''}`));
        });
    });
}

async function fileExists(file) {
    try {
        await access(file);
        return true;
    } catch (error) {
        if (error && typeof error === 'object' && 'code' in error && error.code === 'ENOENT') {
            return false;
        }
        throw error;
    }
}

async function locateBinary(extractDir, archiveName, binaryName) {
    const suffix = '.tar.gz';
    if (!archiveName.endsWith(suffix)) {
        throw new Error(`expected a tar.gz archive, received ${archiveName}`);
    }
    const nested = path.join(extractDir, archiveName.slice(0, -suffix.length), binaryName);
    if (await fileExists(nested)) {
        return nested;
    }
    const flat = path.join(extractDir, binaryName);
    if (await fileExists(flat)) {
        return flat;
    }
    throw new Error(`extracted wasm-pack archive has no ${binaryName}`);
}

async function fetchRelease(fetchImpl, url) {
    const response = await fetchImpl(url, { redirect: 'follow' });
    const status = response && typeof response.status === 'number' ? response.status : 'none';
    if (status !== 200) {
        throw new Error(`wasm-pack download failed with HTTP ${status}`);
    }
    if (!response.body) {
        throw new Error('wasm-pack download returned an empty body');
    }
    return response;
}

async function appendGitHubPath(githubPath, directory) {
    let prefix = '';
    try {
        const current = await readFile(githubPath, 'utf8');
        if (current.length > 0 && !current.endsWith('\n')) {
            prefix = '\n';
        }
    } catch (error) {
        if (!(error && typeof error === 'object' && 'code' in error && error.code === 'ENOENT')) {
            throw error;
        }
    }
    await appendFile(githubPath, `${prefix}${directory}\n`, 'utf8');
}

export async function installWasmPack({
    platform = process.platform,
    arch = process.arch,
    env = process.env,
    fetchImpl = globalThis.fetch,
    readInstalledVersion = readWasmPackVersion,
    destDir
} = {}) {
    if (installedWasmPackMatches(readInstalledVersion())) {
        return { skipped: true };
    }
    if (typeof fetchImpl !== 'function') {
        throw new Error('fetch is required to download wasm-pack');
    }
    const asset = selectWasmPackTarget(platform, arch);
    const response = await fetchRelease(fetchImpl, asset.url);
    const root = path.resolve(
        destDir ?? path.join(env.RUNNER_TEMP || os.tmpdir(), `moyodb-wasm-pack-${WASM_PACK_VERSION}`)
    );
    const extractDir = path.join(root, 'extract');
    const binDir = path.join(root, 'bin');
    await mkdir(extractDir, { recursive: true });
    await mkdir(binDir, { recursive: true });
    const archivePath = path.join(root, asset.archiveName);
    await pipeline(Readable.fromWeb(response.body), createWriteStream(archivePath));
    await extractTarGz(archivePath, extractDir);
    const source = await locateBinary(extractDir, asset.archiveName, asset.binaryName);
    const binaryPath = path.join(binDir, asset.binaryName);
    await copyFile(source, binaryPath);
    if (platform !== 'win32') {
        await chmod(binaryPath, 0o755);
    }
    const binaryStat = await stat(binaryPath);
    if (binaryStat.size <= 0) {
        throw new Error(`downloaded wasm-pack binary is empty: ${binaryPath}`);
    }
    await rm(archivePath, { force: true });
    await rm(extractDir, { recursive: true, force: true });
    const githubPath = env.GITHUB_PATH;
    if (typeof githubPath === 'string' && githubPath.length > 0) {
        await appendGitHubPath(githubPath, binDir);
    }
    return { skipped: false, binDir, binaryPath };
}

function invokedAsCli() {
    const entry = process.argv[1];
    if (typeof entry !== 'string' || entry.length === 0) {
        return false;
    }
    const left = path.resolve(entry);
    const right = path.resolve(fileURLToPath(import.meta.url));
    if (process.platform === 'win32') {
        return left.toLowerCase() === right.toLowerCase();
    }
    return left === right;
}

if (invokedAsCli()) {
    try {
        const result = await installWasmPack();
        if (result.skipped) {
            console.log(`wasm-pack ${WASM_PACK_VERSION} is already on PATH`);
        } else {
            console.log(`installed wasm-pack ${WASM_PACK_VERSION} into ${result.binDir}`);
        }
    } catch (error) {
        console.error(error instanceof Error ? (error.stack ?? error.message) : String(error));
        process.exitCode = 1;
    }
}
