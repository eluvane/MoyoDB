#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync } from 'node:fs';

const configPath = '.config/moyo/ci/actionlint.yml';
if (!existsSync(configPath)) {
    console.error(`Missing ${configPath}`);
    process.exit(1);
}

const text = readFileSync(configPath, 'utf8');
const shellcheck = !/^shellcheck:\s*false\s*$/mu.test(text);
const ignorePatterns = [];
let inIgnore = false;
for (const line of text.split(/\r?\n/)) {
    if (/^ignore:\s*\[\]\s*$/.test(line)) {
        inIgnore = false;
        continue;
    }
    if (/^ignore:\s*$/.test(line)) {
        inIgnore = true;
        continue;
    }
    const match = inIgnore && line.match(/^\s*-\s*["']?(.+?)["']?\s*$/);
    if (match) {
        ignorePatterns.push(match[1]);
    }
}

const workflows = readdirSync('.github/workflows')
    .filter((entry) => /\.ya?ml$/u.test(entry))
    .map((entry) => `.github/workflows/${entry}`)
    .sort();
if (workflows.length === 0) {
    console.error('No GitHub Actions workflows found under .github/workflows.');
    process.exit(1);
}

const args = [];
if (!shellcheck) {
    args.push('-shellcheck=');
}
for (const pattern of ignorePatterns) {
    args.push('-ignore', pattern);
}
args.push(...workflows);

const localActionlint = 'node_modules/actionlint/actionlint.cjs';
const command = existsSync(localActionlint) ? process.execPath : 'actionlint';
const finalArgs = existsSync(localActionlint) ? [localActionlint, ...args] : args;
const result = spawnSync(command, finalArgs, { stdio: 'inherit' });
if (result.error) {
    console.error(result.error.message);
    process.exit(1);
}
process.exit(result.status ?? 1);
