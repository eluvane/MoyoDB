import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import ts from 'typescript';

export function readFlagValue(args, flag) {
    const index = args.indexOf(flag);
    if (index < 0) return undefined;
    if (!args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`missing ${flag} value`);
    return args[index + 1];
}

export async function emitTranspiledModule({
    name,
    input,
    sourceText,
    outputDir,
    sourceRoot,
    reportErrors,
    transformers
}) {
    const text = sourceText ?? (await readFile(input, 'utf8'));
    const transpileOptions = {
        fileName: `${name}.ts`,
        reportDiagnostics: true,
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
    };
    if (transformers !== undefined) transpileOptions.transformers = transformers;
    const result = ts.transpileModule(text, transpileOptions);
    const errors = (result.diagnostics ?? []).filter(
        (diagnostic) => diagnostic.category === ts.DiagnosticCategory.Error
    );
    if (reportErrors === 'assert') {
        assert.equal(errors.length, 0);
    } else if (reportErrors === 'throw') {
        if (errors.length) {
            throw new Error(
                ts.formatDiagnosticsWithColorAndContext(errors, {
                    getCanonicalFileName: (file) => file,
                    getCurrentDirectory: () => sourceRoot,
                    getNewLine: () => '\n'
                })
            );
        }
    } else {
        throw new Error(`unsupported diagnostic report ${reportErrors}`);
    }
    await writeFile(join(outputDir, `${name}.mjs`), result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'"));
}
