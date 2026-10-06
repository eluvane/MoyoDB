import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import ts from 'typescript';
import { createTransportSuite } from '../tests/transport-work-suite.mjs';
import { emitTranspiledModule, readFlagValue } from './fixture-emit.mjs';

// Tests production client, server, protocol and scheduler without a browser, WASM or OPFS.
// Native structuredClone preserves buffer copy and transfer behavior at the message boundary.
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const source = resolve(readFlagValue(args, '--source-root') ?? join(here, '../src'));
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
        // Count then registrations in emitted test code. Evaluate each receiver
        // once and leave Promise methods and native prototypes unchanged.
        const prefix = instrumented
            ? `export const __transportWorkCounter = { thenRegistrations: 0 };
function __transportWorkReceiver(receiver) {
    __transportWorkCounter.thenRegistrations += 1;
    return receiver;
}
`
            : '';
        await emitTranspiledModule({
            name,
            sourceText: prefix + input,
            outputDir: output,
            sourceRoot: source,
            reportErrors: 'throw',
            transformers: instrumented ? { before: [countThenCalls] } : undefined
        });
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
