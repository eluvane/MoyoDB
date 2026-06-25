#!/usr/bin/env node
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';

const root = 'proofs/moyodb_proofs';
const toolchainPath = join(root, 'lean-toolchain');
const manifestPath = join(root, 'lake-manifest.json');
const lakefilePath = join(root, 'lakefile.lean');
const failures = [];

if (!existsSync(toolchainPath)) {
    failures.push('Missing Lean toolchain file proofs/moyodb_proofs/lean-toolchain.');
} else {
    const toolchain = readFileSync(toolchainPath, 'utf8').trim();
    if (toolchain !== 'leanprover/lean4:v4.31.0') {
        failures.push(`Lean toolchain must be pinned to leanprover/lean4:v4.31.0, got ${JSON.stringify(toolchain)}.`);
    }
}

if (!existsSync(lakefilePath)) {
    failures.push('Missing Lake configuration proofs/moyodb_proofs/lakefile.lean.');
} else {
    const lakefile = readFileSync(lakefilePath, 'utf8');
    if (!/builtinLint\s*:=\s*true/u.test(lakefile)) {
        failures.push(
            'proofs/moyodb_proofs/lakefile.lean must enable builtinLint := true so lake lint runs built-in Lean linters.'
        );
    }
}

if (!existsSync(manifestPath)) {
    failures.push('Missing committed Lake manifest proofs/moyodb_proofs/lake-manifest.json.');
} else {
    try {
        JSON.parse(readFileSync(manifestPath, 'utf8'));
    } catch (error) {
        failures.push(`Invalid lake-manifest.json: ${error.message}`);
    }
}

function collectLeanFiles(dir, files = []) {
    for (const entry of readdirSync(dir)) {
        if (['.lake', 'build'].includes(entry)) continue;
        const path = join(dir, entry);
        const stat = statSync(path);
        if (stat.isDirectory()) collectLeanFiles(path, files);
        else if (path.endsWith('.lean')) files.push(path);
    }
    return files;
}

const banned = [
    { regex: /\bsorry\b/u, label: 'sorry placeholder' },
    { regex: /\badmit\b/u, label: 'admit placeholder' },
    { regex: /\bsorryAx\b/u, label: 'sorryAx dependency marker' },
    { regex: /^\s*axiom\b/mu, label: 'custom axiom declaration' },
    { regex: /^\s*unsafe\b/mu, label: 'unsafe top-level declaration' },
    { regex: /^\s*partial\s+def\b/mu, label: 'partial definition' },
    { regex: /#eval!/u, label: '#eval! proof-hole bypass' },
    { regex: /^\s*import\s+all\b/mu, label: 'import all' },
    { regex: /set_option\s+autoImplicit\s+true/u, label: 'autoImplicit true' },
    { regex: /set_option\s+linter\.[A-Za-z0-9_.]+\s+false/u, label: 'disabled Lean linter' }
];

for (const file of collectLeanFiles(root)) {
    const source = readFileSync(file, 'utf8');
    for (const rule of banned) {
        if (rule.regex.test(source)) failures.push(`${file}: contains ${rule.label}`);
    }
}

if (failures.length > 0) {
    console.error(failures.join('\n'));
    process.exit(1);
}
console.log('Lean source hygiene policy passed.');
