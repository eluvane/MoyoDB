import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { ESLint } from 'eslint';
import tseslint from 'typescript-eslint';

const eslint = new ESLint({
    overrideConfigFile: fileURLToPath(new URL('../lints/eslint.config.mjs', import.meta.url)),
    overrideConfig: tseslint.configs.disableTypeChecked
});
const policyRules = new Set(['project/esm-only', 'no-restricted-syntax']);

async function policyMessages(source, filePath = 'packages/sdk/src/policy-witness.ts') {
    const [result] = await eslint.lintText(source, { filePath });
    assert.equal(result.fatalErrorCount, 0, JSON.stringify(result.messages));
    return result.messages.filter((message) => policyRules.has(message.ruleId));
}

test('ESM policy rejects CommonJS values and TypeScript references', async () => {
    for (const source of [
        'require("x");',
        'module.exports = value;',
        'exports.value = value;',
        '__dirname;',
        '__filename;',
        'type T = typeof require;',
        'let value: require;',
        'interface X extends module {}'
    ]) {
        const messages = await policyMessages(source);
        assert.equal(messages.length, 1, source);
        assert.equal(messages[0].ruleId, 'project/esm-only', source);
        assert.equal(messages[0].severity, 2, source);
    }
});

test('ESM policy allows local bindings and property names', async () => {
    for (const source of [
        'function f(require) { return require("x"); }',
        'const require = local; require("x");',
        'import require from "node:module"; require("x");',
        'const object = { require: true, module: true }; object.require;',
        'type require = string; let value: require;',
        'interface module {}',
        'function f(__dirname) { return __dirname; }'
    ]) {
        assert.deepEqual(await policyMessages(source), [], source);
    }
});

test('ESM policy rejects returns outside functions', async () => {
    for (const source of ['return 1;', 'namespace N { return 1; }']) {
        const messages = await policyMessages(source);
        assert.equal(messages.length, 1, source);
        assert.equal(messages[0].ruleId, 'project/esm-only', source);
        assert.equal(messages[0].message, 'Top-level return is forbidden in ESM.', source);
    }
});

test('ESM policy allows returns inside functions', async () => {
    for (const source of [
        'function f() { return 1; }',
        'const f = () => { return 1; };',
        'class C { f() { return 1; } }'
    ]) {
        assert.deepEqual(await policyMessages(source), [], source);
    }
});

test('ESM policy rejects redundant strict directives', async () => {
    for (const source of ['"use strict";', 'function f() { "use strict"; }']) {
        const messages = await policyMessages(source);
        assert.equal(messages.length, 1, source);
        assert.equal(messages[0].ruleId, 'project/esm-only', source);
    }
    assert.deepEqual(await policyMessages('const value = "use strict";'), []);
});

test('process exit policy preserves direct, optional and named property checks', async () => {
    for (const source of [
        'process.exit(0);',
        'process?.exit(0);',
        'process.exit?.(0);',
        'process[exit](0);',
        'function f(process) { process.exit(0); }',
        'import process from "node:process"; process.exit(0);'
    ]) {
        const messages = await policyMessages(source);
        assert.equal(messages.length, 1, source);
        assert.equal(messages[0].ruleId, 'no-restricted-syntax', source);
        assert.equal(messages[0].message, "Don't use process.exit(); throw an error instead.", source);
    }
});

test('process exit policy preserves allowed accesses', async () => {
    for (const source of [
        'process.exitCode = 1;',
        'process["exit"](0);',
        'import { exit } from "node:process"; exit(0);',
        'const object = { exit() {} }; object.exit(0);'
    ]) {
        assert.deepEqual(await policyMessages(source), [], source);
    }
});

test('local policies retain their TypeScript scope', async () => {
    assert.deepEqual(await policyMessages('require("x"); process.exit(0);', '.config/moyo/quality/witness.mjs'), []);
});

test('formatting policy disables multiline conflicts for TypeScript and scripts', async () => {
    for (const file of ['packages/sdk/src/index.ts', '.config/moyo/quality/check-json-files.mjs']) {
        const config = await eslint.calculateConfigForFile(file);
        assert.deepEqual(config.rules['no-unexpected-multiline'], [0], file);
    }
});
