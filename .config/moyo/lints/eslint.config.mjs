import js from '@eslint/js';
import globals from 'globals';
import importX from 'eslint-plugin-import-x';
import promise from 'eslint-plugin-promise';
import regexp from 'eslint-plugin-regexp';
import security from 'eslint-plugin-security';
import tseslint from 'typescript-eslint';
import { fileURLToPath } from 'node:url';

const tsFiles = ['packages/sdk/**/*.ts', 'packages/sdk/*.ts'];
const nodeScriptFiles = ['packages/sdk/**/*.mjs', '.config/moyo/**/*.mjs', 'scripts/**/*.mjs'];
const tsProject = fileURLToPath(new URL('../typescript/tsconfig.eslint.json', import.meta.url));
const typedRecommended = tseslint.configs.recommendedTypeChecked.map((config) => ({
    ...config,
    files: tsFiles
}));

const commonJsGlobals = new Set(['exports', 'require', 'module', '__filename', '__dirname']);
const functionTypes = new Set(['FunctionDeclaration', 'FunctionExpression', 'ArrowFunctionExpression']);
const esmOnly = {
    meta: { type: 'problem', schema: [] },
    create(context) {
        return {
            'ExpressionStatement[directive="use strict"]'(node) {
                context.report({ node, message: 'ESM does not need a use strict directive.' });
            },
            ReturnStatement(node) {
                if (!context.sourceCode.getAncestors(node).some((ancestor) => functionTypes.has(ancestor.type))) {
                    context.report({
                        node: context.sourceCode.getFirstToken(node),
                        message: 'Top-level return is forbidden in ESM.'
                    });
                }
            },
            Identifier(node) {
                if (commonJsGlobals.has(node.name) && context.sourceCode.isGlobalReference(node)) {
                    context.report({ node, message: `CommonJS global ${node.name} is forbidden in ESM.` });
                }
            }
        };
    }
};

export default tseslint.config(
    {
        ignores: [
            '**/node_modules/**',
            '**/dist/**',
            '**/coverage/**',
            '**/.lake/**',
            '**/target/**',
            '**/public/engine/**',
            '**/package-lock.json',
            'proofs/artifacts/**'
        ]
    },
    js.configs.recommended,
    ...typedRecommended,
    importX.flatConfigs.recommended,
    importX.flatConfigs.typescript,
    promise.configs['flat/recommended'],
    regexp.configs['flat/recommended'],
    security.configs.recommended,
    {
        files: tsFiles,
        languageOptions: {
            parserOptions: {
                project: tsProject,
                tsconfigRootDir: process.cwd()
            },
            globals: {
                ...globals.browser,
                ...globals.es2024,
                ...globals.node
            }
        },
        plugins: { project: { rules: { 'esm-only': esmOnly } } },
        settings: {
            'import-x/resolver': {
                typescript: {
                    project: 'packages/sdk/tsconfig.json'
                },
                node: true
            }
        },
        rules: {
            '@typescript-eslint/consistent-type-imports': [
                'warn',
                { prefer: 'type-imports', fixStyle: 'separate-type-imports' }
            ],
            '@typescript-eslint/no-explicit-any': 'warn',
            '@typescript-eslint/no-floating-promises': ['error', { ignoreVoid: true }],
            '@typescript-eslint/await-thenable': 'warn',
            '@typescript-eslint/no-base-to-string': 'warn',
            '@typescript-eslint/no-duplicate-type-constituents': 'warn',
            '@typescript-eslint/no-redundant-type-constituents': 'warn',
            '@typescript-eslint/only-throw-error': 'warn',
            '@typescript-eslint/no-implied-eval': 'warn',
            '@typescript-eslint/no-misused-promises': ['error', { checksVoidReturn: { attributes: false } }],
            '@typescript-eslint/no-non-null-assertion': 'warn',
            '@typescript-eslint/no-unsafe-argument': 'warn',
            '@typescript-eslint/no-unsafe-assignment': 'warn',
            '@typescript-eslint/no-unsafe-call': 'warn',
            '@typescript-eslint/no-unsafe-member-access': 'warn',
            '@typescript-eslint/no-unsafe-return': 'warn',
            '@typescript-eslint/no-unnecessary-condition': 'warn',
            '@typescript-eslint/no-unnecessary-type-assertion': 'warn',
            '@typescript-eslint/no-unused-vars': [
                'error',
                { argsIgnorePattern: '^_', varsIgnorePattern: '^_', caughtErrorsIgnorePattern: '^_' }
            ],
            '@typescript-eslint/prefer-promise-reject-errors': 'warn',
            '@typescript-eslint/unbound-method': 'warn',
            '@typescript-eslint/require-await': 'warn',
            '@typescript-eslint/restrict-template-expressions': 'off',
            'import-x/no-cycle': ['error', { maxDepth: 8 }],
            'import-x/no-deprecated': 'warn',
            'import-x/no-extraneous-dependencies': [
                'error',
                {
                    packageDir: ['.', './packages/sdk'],
                    devDependencies: [
                        'packages/sdk/tests/**',
                        'packages/sdk/bench/**',
                        'packages/sdk/scripts/**',
                        'packages/sdk/*.config.ts'
                    ]
                }
            ],
            'import-x/no-mutable-exports': 'error',
            'import-x/no-useless-path-segments': 'error',
            'no-restricted-syntax': [
                'error',
                {
                    selector:
                        'CallExpression[callee.type="MemberExpression"][callee.object.name="process"][callee.property.name="exit"]',
                    message: "Don't use process.exit(); throw an error instead."
                }
            ],
            'no-console': ['warn', { allow: ['warn', 'error', 'info'] }],
            'no-debugger': 'error',
            'no-empty': ['warn', { allowEmptyCatch: true }],
            'no-implicit-coercion': 'warn',
            'no-useless-assignment': 'warn',
            'no-restricted-imports': [
                'error',
                {
                    patterns: [
                        {
                            group: ['../tests/*', '../bench/*', '../scripts/*'],
                            message: 'Production SDK code must not import tests, benchmarks, or scripts.'
                        }
                    ]
                }
            ],
            'promise/always-return': 'off',
            'promise/catch-or-return': 'off',
            'regexp/no-super-linear-backtracking': 'warn',
            'regexp/optimal-quantifier-concatenation': 'warn',
            'regexp/use-ignore-case': 'warn',
            'security/detect-object-injection': 'off',
            'security/detect-unsafe-regex': 'warn',
            'project/esm-only': 'error',
            'prefer-const': 'warn'
        }
    },
    {
        files: nodeScriptFiles,
        languageOptions: {
            globals: {
                ...globals.node,
                ...globals.es2024
            }
        },
        rules: {
            'import-x/no-unresolved': 'off',
            'no-console': 'off',
            'security/detect-non-literal-fs-filename': 'warn'
        }
    },
    { rules: { 'no-unexpected-multiline': 'off' } }
);
