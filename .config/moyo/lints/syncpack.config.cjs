/** @type {import('syncpack').RcFile} */
module.exports = {
    indent: '  ',
    semverGroups: [
        {
            label: 'Exact npm dependency versions only',
            range: ''
        }
    ],
    sortAz: ['dependencies', 'devDependencies', 'peerDependencies', 'resolutions'],
    source: ['package.json', 'packages/*/package.json'],
    versionGroups: [
        {
            // Native TypeScript 7 CLI (Go). No programmatic API until 7.1.
            label: 'TypeScript 7 native compiler CLI',
            dependencies: ['@typescript/native'],
            packages: ['**'],
            pinVersion: 'npm:typescript@7.0.2'
        },
        {
            // TypeScript 6 API package for tools that import `typescript` (eslint, etc.).
            label: 'TypeScript 6 API for tooling side-by-side with TS7',
            dependencies: ['typescript'],
            packages: ['**'],
            pinVersion: 'npm:@typescript/typescript6@6.0.2'
        }
    ]
};
