import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const temporaryRoot = fileURLToPath(new URL('../../../.tmp/', import.meta.url));
await mkdir(temporaryRoot, { recursive: true });
const directory = await mkdtemp(join(temporaryRoot, 'package-types-'));
try {
    for (const runtime of ['node', 'browser']) {
        const entry = runtime === 'node' ? '@moyodb/sdk/node' : '@moyodb/sdk';
        const open = runtime === 'node' ? 'openNodeDB' : 'openDB';
        const options = runtime === 'node' ? "{ directory: './data' }" : "{ workerMode: 'shared' }";
        const path = join(directory, `${runtime}.mts`);
        await writeFile(
            path,
            `import { ${open}, createSqlClient, openRecordStore, keyCodecs, jsonRecordCodec } from '${entry}';
const db = await ${open}('types', ${options});
await createSqlClient(db).query('SELECT id FROM data');
openRecordStore(db, 'data', { key: keyCodecs.string, value: jsonRecordCodec<{ id: number }>() });
await db.close();
`
        );
        const optionsForCompiler = {
            noEmit: true,
            strict: true,
            target: ts.ScriptTarget.ES2022,
            module: runtime === 'node' ? ts.ModuleKind.NodeNext : ts.ModuleKind.ESNext,
            moduleResolution: runtime === 'node' ? ts.ModuleResolutionKind.NodeNext : ts.ModuleResolutionKind.Bundler,
            lib: runtime === 'node' ? ['lib.es2022.d.ts'] : ['lib.es2022.d.ts', 'lib.dom.d.ts'],
            types: runtime === 'node' ? ['node'] : []
        };
        const program = ts.createProgram([path], optionsForCompiler);
        const diagnostics = ts.getPreEmitDiagnostics(program);
        if (diagnostics.length) {
            throw new Error(
                ts.formatDiagnosticsWithColorAndContext(diagnostics, {
                    getCanonicalFileName: (name) => name,
                    getCurrentDirectory: () => process.cwd(),
                    getNewLine: () => '\n'
                })
            );
        }
    }
    process.stdout.write('Package declarations: NodeNext and browser consumers passed\n');
} finally {
    await rm(directory, { recursive: true, force: true });
}
