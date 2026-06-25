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
            label: 'Use one TypeScript version across the workspace',
            dependencies: ['typescript'],
            packages: ['**'],
            pinVersion: '6.0.3'
        }
    ]
};
