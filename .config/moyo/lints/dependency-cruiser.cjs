/** @type {import('dependency-cruiser').IConfiguration} */
module.exports = {
    forbidden: [
        {
            name: 'no-circular-dependencies',
            severity: 'error',
            comment: 'Cycles make the SDK hard to tree-shake and reason about.',
            from: {},
            to: { circular: true }
        },
        {
            name: 'no-unresolvable-imports',
            severity: 'error',
            from: {},
            to: { couldNotResolve: true }
        },
        {
            name: 'no-production-to-tests',
            severity: 'error',
            from: { path: '^packages/sdk/src' },
            to: { path: '^packages/sdk/(tests|bench|scripts)' }
        },
        {
            name: 'no-test-to-dist-or-generated-wasm',
            severity: 'error',
            from: { path: '^packages/sdk/(src|tests|bench)' },
            to: { path: '^packages/sdk/(dist|public/engine)' }
        },
        {
            name: 'no-rust-or-lean-imports-from-sdk',
            severity: 'error',
            from: { path: '^packages/sdk' },
            to: { path: '^(crates|proofs)/' }
        },
        {
            name: 'no-unknown-npm-runtime-dependencies',
            severity: 'error',
            from: { path: '^packages/sdk/src' },
            to: { dependencyTypes: ['npm-no-pkg', 'npm-unknown', 'npm-dev'] }
        },
        {
            name: 'no-deprecated-node-core',
            severity: 'error',
            from: {},
            to: { dependencyTypes: ['core'], path: '^(domain|punycode|sys)$' }
        },
        {
            name: 'no-path-traversal-out-of-sdk-src',
            severity: 'error',
            from: { path: '^packages/sdk/src' },
            to: { path: '^(?!packages/sdk/src|packages/sdk/package\\.json)' }
        }
    ],
    options: {
        cache: false,
        combinedDependencies: true,
        doNotFollow: {
            path: 'node_modules|dist|coverage|target|\\.lake|packages/sdk/public/engine'
        },
        enhancedResolveOptions: {
            conditionNames: ['import', 'types', 'browser', 'default'],
            extensions: ['.ts', '.tsx', '.mts', '.mjs', '.js', '.json']
        },
        exclude: {
            path: 'node_modules|dist|coverage|target|\\.lake|packages/sdk/public/engine'
        },
        includeOnly: '^packages/sdk',
        reporterOptions: {
            dot: {
                collapsePattern: 'node_modules/[^/]+'
            }
        }
    }
};
