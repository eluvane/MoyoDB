import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, relative, isAbsolute } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { emitTranspiledModule, readFlagValue } from './fixture-emit.mjs';

// Tests production codecs against reference behavior without a browser, storage or WASM build.
const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const source = resolve(readFlagValue(args, '--source-root') ?? join(here, '../src'));
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
        await emitTranspiledModule({
            name,
            input,
            outputDir: output,
            sourceRoot: source,
            reportErrors: 'throw'
        });
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
