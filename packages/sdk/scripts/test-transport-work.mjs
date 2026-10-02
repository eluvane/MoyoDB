import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';
import { createTransportSuite } from '../tests/transport-work-suite.mjs';

// Exercises the real client, server, protocol and scheduler without a browser,
// WASM, OPFS, a build, or benchmark timing. Native structuredClone accounts for
// the buffers that the message boundary actually copies and transfers.
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const sourceIndex = args.indexOf('--source-root');
if (sourceIndex >= 0 && (!args[sourceIndex + 1] || args[sourceIndex + 1].startsWith('--'))) {
    throw new Error('missing --source-root value');
}
const source = resolve(sourceIndex < 0 ? join(here, '../src') : args[sourceIndex + 1]);
const output = await mkdtemp(join(tmpdir(), 'moyo-transport-work-'));

function countThenCalls(context) {
    const visit = (node) => {
        const visited = ts.visitEachChild(node, visit, context);
        if (
            ts.isCallExpression(visited) &&
            ts.isPropertyAccessExpression(visited.expression) &&
            visited.expression.name.text === 'then'
        ) {
            const expression = visited.expression;
            const receiver = context.factory.createCallExpression(
                context.factory.createIdentifier('__transportWorkReceiver'),
                undefined,
                [expression.expression]
            );
            return context.factory.updateCallExpression(
                visited,
                context.factory.updatePropertyAccessExpression(expression, receiver, expression.name),
                visited.typeArguments,
                visited.arguments
            );
        }
        return visited;
    };
    return (sourceFile) => ts.visitNode(sourceFile, visit);
}

try {
    for (const name of ['worker-client', 'worker-server', 'worker-protocol', 'internal']) {
        const input = await readFile(join(source, `${name}.ts`), 'utf8');
        const instrumented = name === 'worker-server';
        // A test-only AST receiver hook observes actual registrations. It
        // returns the same receiver, evaluated once, without touching Promise
        // methods, native prototypes, or production source files.
        const prefix = instrumented
            ? `export const __transportWorkCounter = { thenRegistrations: 0 };
function __transportWorkReceiver(receiver) {
    __transportWorkCounter.thenRegistrations += 1;
    return receiver;
}
`
            : '';
        const result = ts.transpileModule(prefix + input, {
            fileName: `${name}.ts`,
            reportDiagnostics: true,
            compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
            transformers: instrumented ? { before: [countThenCalls] } : undefined
        });
        const errors = (result.diagnostics ?? []).filter((item) => item.category === ts.DiagnosticCategory.Error);
        if (errors.length > 0) {
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
    const [client, server, protocol] = await Promise.all(
        ['worker-client', 'worker-server', 'worker-protocol'].map(
            (name) => import(pathToFileURL(join(output, `${name}.mjs`)).href)
        )
    );
    const mode = args.includes('--semantics-only')
        ? 'semantics'
        : args.includes('--work-only')
          ? 'work'
          : args.includes('--regressions-only')
            ? 'regressions'
            : 'all';
    const suite = createTransportSuite({ ...client, ...server, protocol });
    const result = await suite.runTests(mode);
    console.log(JSON.stringify({ source, node: process.version, typescript: ts.version, mode, ...result }, null, 2));
    if (result.failed > 0) process.exitCode = 1;
} finally {
    await rm(output, { recursive: true, force: true });
}
