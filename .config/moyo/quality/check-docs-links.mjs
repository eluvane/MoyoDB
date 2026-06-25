#!/usr/bin/env node
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, normalize } from 'node:path';

const rootPackage = JSON.parse(readFileSync('package.json', 'utf8'));
const sdkPackage = JSON.parse(readFileSync('packages/sdk/package.json', 'utf8'));
const commandsDocPath = 'docs/COMMANDS.md';
const commandsDoc = existsSync(commandsDocPath) ? readFileSync(commandsDocPath, 'utf8') : '';

const requiredDocs = ['README.md', '.config/moyo/README.md', 'packages/sdk/README.md'];

const requiredRootScripts = [
    'format',
    'format:check',
    'lint',
    'lint:typescript',
    'lint:rust',
    'lint:lean',
    'lint:repo',
    'quality',
    'security',
    'test',
    'build',
    'check',
    'check:all'
];

const failures = [];
for (const doc of requiredDocs) {
    if (!existsSync(doc)) failures.push(`Missing required documentation file: ${doc}`);
}
for (const script of requiredRootScripts) {
    if (!rootPackage.scripts?.[script]) {
        failures.push(`Root package.json is missing script ${script}.`);
    }
    if (commandsDoc && !commandsDoc.includes(`npm run ${script}`)) {
        failures.push(`docs/COMMANDS.md does not document root command: npm run ${script}`);
    }
}
for (const script of Object.keys(sdkPackage.scripts ?? {})) {
    if (commandsDoc && !commandsDoc.includes(`npm run ${script}`)) {
        failures.push(`docs/COMMANDS.md does not document SDK command: npm run ${script}`);
    }
}

function collectMarkdown(dir, files = []) {
    for (const entry of readdirSync(dir)) {
        if (['.git', 'node_modules', 'target', '.lake', 'dist', 'public'].includes(entry)) continue;
        const path = join(dir, entry);
        const stat = statSync(path);
        if (stat.isDirectory()) collectMarkdown(path, files);
        else if (path.endsWith('.md')) files.push(path);
    }
    return files;
}

const localLinkPattern = /\[[^\]]+\]\((?!https?:|mailto:|#)([^)]+)\)/gu;
for (const file of collectMarkdown('.')) {
    const text = readFileSync(file, 'utf8');
    for (const match of text.matchAll(localLinkPattern)) {
        const target = decodeURIComponent(match[1].split('#')[0].trim());
        if (!target) continue;
        const resolved = normalize(join(dirname(file), target));
        if (!existsSync(resolved)) {
            failures.push(`${file}: broken local markdown link -> ${match[1]}`);
        }
    }
}

if (failures.length > 0) {
    console.error(failures.join('\n'));
    process.exit(1);
}
if (!commandsDoc) {
    console.warn('Documentation command coverage skipped: docs/COMMANDS.md is not present.');
}
console.log('Documentation command/link policy passed.');
