#!/usr/bin/env node
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, normalize } from 'node:path';

const rootPackage = JSON.parse(readFileSync('package.json', 'utf8'));
const sdkPackage = JSON.parse(readFileSync('packages/sdk/package.json', 'utf8'));
const qualityReadmePath = '.config/moyo/README.md';
const qualityReadme = existsSync(qualityReadmePath) ? readFileSync(qualityReadmePath, 'utf8') : '';
const commandsDocPath = 'docs/COMMANDS.md';
const commandsDoc = existsSync(commandsDocPath) ? readFileSync(commandsDocPath, 'utf8') : '';

function documentsCommand(text, script) {
    const body = script.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    return new RegExp(`npm run ${body}(?![\\w:-])`, 'u').test(text);
}

const requiredDocs = ['README.md', '.config/moyo/README.md'];

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
    if (qualityReadme && !documentsCommand(qualityReadme, script)) {
        failures.push(`.config/moyo/README.md does not document root command: npm run ${script}`);
    }
    if (commandsDoc && !documentsCommand(commandsDoc, script)) {
        failures.push(`docs/COMMANDS.md does not document root command: npm run ${script}`);
    }
}
// docs/COMMANDS.md is optional. When present, it must name SDK scripts exactly.
for (const script of Object.keys(sdkPackage.scripts ?? {})) {
    if (commandsDoc && !documentsCommand(commandsDoc, script)) {
        failures.push(`docs/COMMANDS.md does not document SDK command: npm run ${script}`);
    }
}

function collectMarkdown(dir, files = []) {
    for (const entry of readdirSync(dir)) {
        if (['.git', '.tmp', 'node_modules', 'target', '.lake', 'dist', 'public'].includes(entry)) continue;
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
console.log('Documentation command/link policy passed.');
