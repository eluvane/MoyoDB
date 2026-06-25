#!/usr/bin/env node
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const ignoredDirs = new Set([
    '.git',
    '.lake',
    '.npm',
    'coverage',
    'dist',
    'node_modules',
    'target',
    'test-results',
    'playwright-report'
]);

function stripJsonc(input) {
    let output = '';
    let inString = false;
    let escaped = false;
    for (let i = 0; i < input.length; i += 1) {
        const char = input[i];
        const next = input[i + 1];
        if (inString) {
            output += char;
            if (escaped) {
                escaped = false;
            } else if (char === '\\') {
                escaped = true;
            } else if (char === '"') {
                inString = false;
            }
            continue;
        }
        if (char === '"') {
            inString = true;
            output += char;
            continue;
        }
        if (char === '/' && next === '/') {
            while (i < input.length && input[i] !== '\n') {
                i += 1;
            }
            output += '\n';
            continue;
        }
        if (char === '/' && next === '*') {
            i += 2;
            while (i < input.length && !(input[i] === '*' && input[i + 1] === '/')) {
                output += input[i] === '\n' ? '\n' : ' ';
                i += 1;
            }
            i += 1;
            continue;
        }
        output += char;
    }
    return output;
}

function collect(dir, files = []) {
    for (const entry of readdirSync(dir)) {
        if (ignoredDirs.has(entry)) continue;
        const path = join(dir, entry);
        const stat = statSync(path);
        if (stat.isDirectory()) {
            collect(path, files);
        } else if (/\.(json|jsonc)$/u.test(path)) {
            files.push(path);
        }
    }
    return files;
}

const failures = [];
for (const file of collect('.')) {
    try {
        const source = readFileSync(file, 'utf8');
        JSON.parse(file.endsWith('.jsonc') ? stripJsonc(source) : source);
    } catch (error) {
        failures.push(`${file}: ${error.message}`);
    }
}

if (failures.length > 0) {
    console.error('Invalid JSON/JSONC files:\n' + failures.join('\n'));
    process.exit(1);
}

console.log('JSON/JSONC validation passed.');
