import { mkdtemp, mkdir, readFile, writeFile, cp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';
import { createSuite } from '../tests/minimum-work-suite.mjs';
// Measures SDK work with transport and compression fixtures. Index codecs,
// metadata ownership, mutation planning and paging use production code.
// WASM and OPFS end-to-end behavior are outside its scope.
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const value = (flag) => {
    const index = args.indexOf(flag);
    if (index < 0) return undefined;
    if (!args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`missing ${flag} value`);
    return args[index + 1];
};
const source = resolve(value('--source-root') ?? join(here, '../src'));
const keep = value('--emit-dir');
const output = keep ? resolve(keep) : await mkdtemp(join(tmpdir(), 'moyo-minimum-work-'));
const modules = ['worker', 'indexing', 'codec', 'errors', 'internal'];
await mkdir(output, { recursive: true });
try {
    for (const name of modules) {
        const input = await readFile(join(source, `${name}.ts`), 'utf8');
        const result = ts.transpileModule(input, {
            fileName: `${name}.ts`,
            reportDiagnostics: true,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
        });
        const errors = (result.diagnostics ?? []).filter((d) => d.category === ts.DiagnosticCategory.Error);
        if (errors.length)
            throw new Error(
                ts.formatDiagnosticsWithColorAndContext(errors, {
                    getCanonicalFileName: (f) => f,
                    getCurrentDirectory: () => source,
                    getNewLine: () => '\n'
                })
            );
        const code = result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'");
        await writeFile(join(output, `${name}.mjs`), code);
    }
    await writeFile(
        join(output, 'worker-server.mjs'),
        `export let runtime;
export function exposeWorkerApi(api) { runtime = api; }
`
    );
    const protocolNames = [
        'packedOptionalValues',
        'unpackPackedBatchOpKeys',
        'unpackPackedBatchOps',
        'unpackPackedBinaryList',
        'unpackPackedBinaryPairKeys',
        'unpackPackedBinaryPairs',
        'unpackPackedOptionalValues'
    ];
    await writeFile(
        join(output, 'worker-protocol.mjs'),
        protocolNames
            .map((name) => `export function ${name}() { throw new Error('packed transport is outside this fixture'); }`)
            .join('\n')
    );
    await writeFile(
        join(output, 'compression.mjs'),
        `
export function compressionFromStoreFlags(flags) {
    if (flags !== 0) throw new Error('compressed format is outside this fixture');
    return false;
}
export async function encodeStoreValueRecord(value, compression) {
    if (compression !== false) throw new Error('compression codec is outside this fixture');
    return value;
}
export function decodeStoreValueRecord() { throw new Error('compression codec is outside this fixture'); }
export function wrapSnapshotWithCompression() { throw new Error('snapshot codec is outside this fixture'); }
export function unwrapSnapshotCompression() { throw new Error('snapshot codec is outside this fixture'); }
`
    );
    await cp(join(here, '../tests/minimum-work-suite.mjs'), join(output, 'suite.mjs'));
    await writeFile(
        join(output, 'suite-loader.mjs'),
        `
import { DbWorker } from './worker.mjs';
import * as indexing from './indexing.mjs';
import * as codec from './codec.mjs';
import { createSuite } from './suite.mjs';
const runtime = new DbWorker();
export const suite = createSuite({ runtime, indexing, codec });
`
    );
    if (keep) {
        console.log(`Portable fixture emitted to ${output}`);
    } else {
        globalThis.self = { addEventListener() {}, removeEventListener() {} };
        const [{ DbWorker }, indexing, codec] = await Promise.all(
            ['worker', 'indexing', 'codec'].map((name) => import(pathToFileURL(join(output, `${name}.mjs`)).href))
        );
        const runtime = new DbWorker();
        const suite = createSuite({ runtime, indexing, codec });
        const mode = args.includes('--measure-only')
            ? 'measure'
            : args.includes('--semantics-only')
              ? 'semantics'
              : 'all';
        const result = mode === 'measure' ? await suite.measureWork() : await suite.runTests(mode === 'all');
        console.log(JSON.stringify({ source, node: process.version, typescript: ts.version, ...result }, null, 2));
        if (result.failed > 0) process.exitCode = 1;
    }
} finally {
    if (!keep) await rm(output, { recursive: true, force: true });
}
