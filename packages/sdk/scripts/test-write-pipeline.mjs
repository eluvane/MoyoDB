import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, relative, isAbsolute } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';
import { checkWritePipeline } from '../tests/write-pipeline-suite.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const source = resolve(here, '../src');
const temporaryRoot = resolve(tmpdir());
const output = await mkdtemp(join(temporaryRoot, 'moyo-write-pipeline-'));
const cleanupPath = relative(temporaryRoot, resolve(output));
if (!cleanupPath || cleanupPath.startsWith('..') || isAbsolute(cleanupPath)) {
    throw new Error('temporary cleanup path escaped its root');
}
try {
    for (const name of [
        'worker',
        'indexing',
        'codec',
        'errors',
        'internal',
        'compression',
        'snappy',
        'worker-protocol'
    ]) {
        const result = ts.transpileModule(await readFile(join(source, `${name}.ts`), 'utf8'), {
            fileName: `${name}.ts`,
            reportDiagnostics: true,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
        });
        const errors = (result.diagnostics ?? []).filter(
            (diagnostic) => diagnostic.category === ts.DiagnosticCategory.Error
        );
        if (errors.length) {
            throw new Error(
                ts.formatDiagnosticsWithColorAndContext(errors, {
                    getCanonicalFileName: (file) => file,
                    getCurrentDirectory: () => source,
                    getNewLine: () => '\n'
                })
            );
        }
        await writeFile(join(output, `${name}.mjs`), result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'"));
    }
    await writeFile(join(output, 'worker-server.mjs'), 'export function exposeWorkerApi() {}\n');
    const [worker, indexing, codec, compression, protocol] = await Promise.all(
        ['worker', 'indexing', 'codec', 'compression', 'worker-protocol'].map(
            (name) => import(pathToFileURL(join(output, `${name}.mjs`)).href)
        )
    );
    const result = await checkWritePipeline({ ...worker, indexing, codec, compression, protocol });
    console.log(JSON.stringify({ node: process.version, ...result }, null, 2));
    if (result.failed > 0) process.exitCode = 1;
} finally {
    await rm(output, { recursive: true, force: true });
}
