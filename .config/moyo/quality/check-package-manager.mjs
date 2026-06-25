#!/usr/bin/env node
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const failures = [];
const rootPackage = JSON.parse(readFileSync('package.json', 'utf8'));

if (!rootPackage.packageManager?.startsWith('npm@')) {
    failures.push('Root package.json must pin packageManager to npm@<version>.');
}
if (!existsSync('package-lock.json')) {
    failures.push('Root package-lock.json is required for npm workspace lockfile consistency.');
}
for (const forbidden of ['pnpm-lock.yaml', 'yarn.lock', 'bun.lockb']) {
    if (existsSync(forbidden)) {
        failures.push(`Unexpected lockfile ${forbidden}; this repo uses npm workspaces.`);
    }
}
for (const workspace of rootPackage.workspaces ?? []) {
    for (const nested of ['package-lock.json', 'pnpm-lock.yaml', 'yarn.lock', 'bun.lockb']) {
        const nestedLock = join(workspace, nested);
        if (existsSync(nestedLock)) {
            failures.push(`Nested lockfile ${nestedLock} is forbidden; keep the single root lockfile authoritative.`);
        }
    }
}
const sdkPackage = JSON.parse(readFileSync('packages/sdk/package.json', 'utf8'));
if (sdkPackage.packageManager) {
    failures.push('packages/sdk/package.json must not override the root packageManager.');
}

if (failures.length > 0) {
    console.error(failures.join('\n'));
    process.exit(1);
}
console.log('Package-manager policy passed.');
