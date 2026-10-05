import { readFileSync } from 'node:fs';
import { parseTree } from 'jsonc-parser';
import { collectLintFiles } from './collect-lint-files.mjs';

const { rules } = JSON.parse(readFileSync('.config/moyo/lints/npm-package-json-lint.json', 'utf8'));
const excluded = readFileSync('.config/moyo/formatters/prettierignore', 'utf8').split(/\r?\n/u);
const files = collectLintFiles((file) => file === 'package.json' || file.endsWith('/package.json'), excluded);
if (files.length === 0) throw new Error('Package lint scope contains no files.');
const requiredProperties = new Set([
    'name',
    'version',
    'description',
    'license',
    'scripts',
    'repository',
    'keywords',
    'engines'
]);
let failures = 0;

function report(file, rule, severity, message) {
    console.error(`${file}: ${severity} ${rule}: ${message}`);
    if (severity === 'error') failures += 1;
}

function duplicateProperties(node) {
    const duplicates = [];
    if (node.type === 'object') {
        const seen = new Set();
        for (const property of node.children ?? []) {
            const [key, value] = property.children;
            if (seen.has(key.value)) duplicates.push(key.value);
            seen.add(key.value);
            duplicates.push(...duplicateProperties(value));
        }
    } else if (node.type === 'array') {
        for (const child of node.children ?? []) duplicates.push(...duplicateProperties(child));
    }
    return duplicates;
}

for (const file of files) {
    const source = readFileSync(file, 'utf8');
    let manifest;
    try {
        manifest = JSON.parse(source);
        if (manifest === null || typeof manifest !== 'object' || Array.isArray(manifest)) {
            throw new Error('package.json must contain an object');
        }
    } catch (error) {
        report(file, 'valid-json', 'error', error.message);
        continue;
    }
    for (const [rule, setting] of Object.entries(rules)) {
        const [severity, options] = Array.isArray(setting) ? setting : [setting];
        if (severity === 'off') continue;
        if (!['error', 'warning'].includes(severity)) throw new Error(`Unsupported severity for ${rule}: ${severity}`);
        if (rule.startsWith('require-') && requiredProperties.has(rule.slice(8))) {
            const property = rule.slice(8);
            if (!Object.hasOwn(manifest, property)) report(file, rule, severity, `${property} is required`);
        } else if (rule === 'valid-values-license') {
            if (!Array.isArray(options)) throw new Error('valid-values-license requires an array');
            if (Object.hasOwn(manifest, 'license') && !options.includes(manifest.license)) {
                report(file, rule, severity, `Invalid license value: ${manifest.license}`);
            }
        } else if (['prefer-alphabetical-dependencies', 'prefer-alphabetical-devDependencies'].includes(rule)) {
            const property = rule.slice('prefer-alphabetical-'.length);
            if (Object.hasOwn(manifest, property)) {
                const keys = Object.keys(manifest[property]);
                const sorted = [...keys].sort();
                if (keys.some((key, index) => key !== sorted[index])) {
                    report(file, rule, severity, `${property} must be in alphabetical order`);
                }
            }
        } else if (rule === 'no-duplicate-properties') {
            const duplicates = duplicateProperties(parseTree(source));
            if (duplicates.length > 0) report(file, rule, severity, `Duplicate properties: ${duplicates.join(', ')}`);
        } else {
            throw new Error(`Unsupported package lint rule: ${rule}`);
        }
    }
}
if (failures > 0) process.exitCode = 1;
else console.log(`Package metadata lint passed: ${files.length} files checked.`);
