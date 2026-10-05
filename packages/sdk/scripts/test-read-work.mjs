import { mkdtemp, mkdir, readFile, writeFile, cp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';
import { createReadSuite } from '../tests/read-work-suite.mjs';

// Tests production read methods, codecs and packet decoding with WASM and
// Worker transport fixtures. Storage and latency are outside its scope.
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
const output = keep ? resolve(keep) : await mkdtemp(join(tmpdir(), 'moyo-read-work-'));
const modules = ['worker', 'indexing', 'codec', 'errors', 'internal', 'compression', 'worker-protocol'];
await mkdir(output, { recursive: true });
let runtime;
const originalSelf = globalThis.self;
try {
    for (const name of modules) {
        const result = ts.transpileModule(await readFile(join(source, `${name}.ts`), 'utf8'), {
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
        await writeFile(join(output, `${name}.mjs`), result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'"));
    }
    await writeFile(
        join(output, 'worker-server.mjs'),
        'export let runtime;\nexport function exposeWorkerApi(api) { runtime = api; }\n'
    );
    await cp(join(here, '../tests/read-work-suite.mjs'), join(output, 'read-suite.mjs'));
    await writeFile(
        join(output, 'read-suite-loader.mjs'),
        `import './worker.mjs';
import { runtime } from './worker-server.mjs';
import * as compression from './compression.mjs';
import { createReadSuite } from './read-suite.mjs';
export const suite = createReadSuite({ runtime, compression });
`
    );
    if (keep) {
        console.log(`Portable read fixture emitted to ${output}`);
    } else {
        globalThis.self = { addEventListener() {}, removeEventListener() {} };
        await import(pathToFileURL(join(output, 'worker.mjs')).href);
        runtime = (await import(pathToFileURL(join(output, 'worker-server.mjs')).href)).runtime;
        const compression = await import(pathToFileURL(join(output, 'compression.mjs')).href);
        const result = await createReadSuite({ runtime, compression }).runTests(!args.includes('--semantics-only'));
        console.log(JSON.stringify({ source, node: process.version, ...result }, null, 2));
        if (result.failed > 0) process.exitCode = 1;
    }
} finally {
    runtime?.persistenceBridge.close();
    if (originalSelf === undefined) delete globalThis.self;
    else globalThis.self = originalSelf;
    if (!keep) await rm(output, { recursive: true, force: true });
}
