#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { closeSync, mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';

const mode = process.argv[2];
if (!['format', 'check', 'lint'].includes(mode)) {
    throw new Error('Expected a Taplo mode: format, check, or lint.');
}
// File descriptors also work when Windows restricts subprocess pipe creation.
function runCaptured(command, args, inputPath) {
    const temporaryRoot = resolve('.tmp');
    mkdirSync(temporaryRoot, { recursive: true });
    const temporaryDirectory = mkdtempSync(join(temporaryRoot, 'taplo-'));
    if (dirname(temporaryDirectory) !== temporaryRoot) throw new Error('Taplo temporary directory escaped .tmp.');
    const descriptors = [];
    try {
        const outputPath = join(temporaryDirectory, 'stdout');
        const errorPath = join(temporaryDirectory, 'stderr');
        const input = inputPath === undefined ? 'ignore' : openSync(inputPath, 'r');
        if (typeof input === 'number') descriptors.push(input);
        const output = openSync(outputPath, 'w');
        descriptors.push(output);
        const error = openSync(errorPath, 'w');
        descriptors.push(error);
        const result = spawnSync(command, args, { stdio: [input, output, error], timeout: 60_000 });
        return { ...result, stdout: readFileSync(outputPath, 'utf8'), stderr: readFileSync(errorPath, 'utf8') };
    } finally {
        for (const descriptor of descriptors) closeSync(descriptor);
        rmSync(temporaryDirectory, { recursive: true, force: true });
    }
}

const listed = runCaptured('git', ['ls-files', '--cached', '--others', '--exclude-standard', '-z', '--', '*.toml']);
if (listed.error) throw listed.error;
if (listed.status !== 0) throw new Error(listed.stderr || 'Failed to list TOML files.');
const files = [...new Set(listed.stdout.split('\0').filter(Boolean))];
if (files.length === 0) throw new Error('No TOML files found.');

const require = createRequire(import.meta.url);
const cli = join(dirname(require.resolve('@taplo/cli/package.json')), 'dist', 'cli.js');
const args = [cli, mode === 'lint' ? 'lint' : 'fmt', '--config', '.config/moyo/formatters/taplo.toml'];
if (mode === 'check') args.push('--check');
args.push('-');

// The npm Taplo glob adapter does not match Windows paths. Feed each source
// through stdin on every platform, retaining the repository's Taplo settings.
for (const file of files) {
    const result = runCaptured(process.execPath, args, file);
    if (result.error) throw result.error;
    if (result.status !== 0) {
        process.stderr.write(`${file}:\n${result.stderr}${result.stdout}`);
        process.exitCode = 1;
    } else if (mode === 'format') {
        writeFileSync(file, result.stdout);
    }
}
process.stdout.write(`Taplo ${mode}: ${files.length} TOML files checked.\n`);
