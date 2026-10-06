import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, relative, isAbsolute } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';

// Tests production codecs against reference behavior without a browser, storage or WASM build.
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const sourceIndex = args.indexOf('--source-root');
if (sourceIndex >= 0 && (!args[sourceIndex + 1] || args[sourceIndex + 1].startsWith('--'))) {
    throw new Error('missing --source-root value');
}
const source = resolve(sourceIndex >= 0 ? args[sourceIndex + 1] : join(here, '../src'));
const temporaryRoot = resolve(tmpdir());
const output = await mkdtemp(join(temporaryRoot, 'moyo-codec-index-work-'));
const relativeOutput = relative(temporaryRoot, resolve(output));
if (!relativeOutput || relativeOutput.startsWith('..') || isAbsolute(relativeOutput)) {
    throw new Error('temporary cleanup path escaped its root');
}
try {
    for (const name of [
        'codec',
        'indexing',
        'compression',
        'snappy',
        'internal',
        'errors',
        'codec-index-work-cases',
        'compression-work-cases'
    ]) {
        const input = name.endsWith('-cases') ? join(here, `../tests/${name}.ts`) : join(source, `${name}.ts`);
        const result = ts.transpileModule(await readFile(input, 'utf8'), {
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
    const [codec, indexing, compression, cases, compressionCases] = await Promise.all(
        ['codec', 'indexing', 'compression', 'codec-index-work-cases', 'compression-work-cases'].map(
            (name) => import(pathToFileURL(join(output, `${name}.mjs`)).href)
        )
    );
    const result = await cases.checkCodecIndexWork({ codec, indexing, compression });
    const baseline = await compressionCases.checkCompressionWork(compression);
    console.log(JSON.stringify({ source, node: process.version, ...result, compressionBaseline: baseline }, null, 2));
    if (result.failed > 0) {
        process.exitCode = 1;
    }
} finally {
    await rm(output, { recursive: true, force: true });
}
