import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, relative, isAbsolute } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';

const here = dirname(fileURLToPath(import.meta.url));
const temporaryRoot = resolve(tmpdir());
const output = await mkdtemp(join(temporaryRoot, 'moyo-record-work-'));
const relativeOutput = relative(temporaryRoot, resolve(output));
if (!relativeOutput || relativeOutput.startsWith('..') || isAbsolute(relativeOutput)) {
    throw new Error('temporary cleanup path escaped its root');
}
try {
    for (const name of ['records', 'codec', 'errors', 'record-work-cases']) {
        const input =
            name === 'record-work-cases' ? join(here, `../tests/${name}.ts`) : join(here, `../src/${name}.ts`);
        const result = ts.transpileModule(await readFile(input, 'utf8'), {
            fileName: `${name}.ts`,
            reportDiagnostics: true,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
        });
        const errors = (result.diagnostics ?? []).filter(
            (diagnostic) => diagnostic.category === ts.DiagnosticCategory.Error
        );
        if (errors.length > 0) {
            throw new Error(
                ts.formatDiagnosticsWithColorAndContext(errors, {
                    getCanonicalFileName: (file) => file,
                    getCurrentDirectory: () => here,
                    getNewLine: () => '\n'
                })
            );
        }
        await writeFile(join(output, `${name}.mjs`), result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'"));
    }
    const [records, codec, cases] = await Promise.all(
        ['records', 'codec', 'record-work-cases'].map((name) => import(pathToFileURL(join(output, `${name}.mjs`)).href))
    );
    const result = await cases.checkRecordWork(records, codec);
    console.log(JSON.stringify({ node: process.version, ...result }, null, 2));
    if (result.failed > 0) {
        process.exitCode = 1;
    }
} finally {
    await rm(output, { recursive: true, force: true });
}
