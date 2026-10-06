import { mkdtemp, mkdir, writeFile, cp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { createReadSuite } from '../tests/read-work-suite.mjs';
import { emitTranspiledModule, readFlagValue } from './fixture-emit.mjs';

// Tests production read methods, codecs and packet decoding with WASM and
// Worker transport fixtures. Storage and latency are outside its scope.
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const source = resolve(readFlagValue(args, '--source-root') ?? join(here, '../src'));
const keep = readFlagValue(args, '--emit-dir');
const output = keep ? resolve(keep) : await mkdtemp(join(tmpdir(), 'moyo-read-work-'));
const modules = ['worker', 'indexing', 'codec', 'errors', 'internal', 'compression', 'snappy', 'worker-protocol'];
await mkdir(output, { recursive: true });
let runtime;
const originalSelf = globalThis.self;
try {
    for (const name of modules) {
        await emitTranspiledModule({
            name,
            input: join(source, `${name}.ts`),
            outputDir: output,
            sourceRoot: source,
            reportErrors: 'throw'
        });
    }
    await writeFile(
        join(output, 'worker-server.mjs'),
        'export let runtime;\nexport function exposeWorkerApi(api) { runtime = api; }\n'
    );
    await cp(join(here, '../tests/read-work-suite.mjs'), join(output, 'read-suite.mjs'));
    await writeFile(
        join(output, 'read-suite-loader.mjs'),
        `import { DbWorker } from './worker.mjs';
import * as compression from './compression.mjs';
import { createReadSuite } from './read-suite.mjs';
const runtime = new DbWorker();
export const suite = createReadSuite({ runtime, compression });
`
    );
    if (keep) {
        console.log(`Portable read fixture emitted to ${output}`);
    } else {
        globalThis.self = { addEventListener() {}, removeEventListener() {} };
        const { DbWorker } = await import(pathToFileURL(join(output, 'worker.mjs')).href);
        runtime = new DbWorker();
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
