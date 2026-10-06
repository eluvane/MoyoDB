import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import ts from 'typescript';
import {
    emitTranspiledModule,
    fixtureEmitCacheFile,
    fixtureEmitCacheKey,
    fixtureEmitStats,
    resetFixtureEmitStats,
    transformerCacheTag
} from './fixture-emit.mjs';

function makeCountThen() {
    return function countThen(context) {
        const visit = (node) => {
            if (ts.isStringLiteral(node) && node.text === 'plain') {
                return context.factory.createStringLiteral('instrumented');
            }
            return ts.visitEachChild(node, visit, context);
        };
        return (sourceFile) => ts.visitNode(sourceFile, visit);
    };
}

test('cache key includes source, compiler options, and transformer tag', () => {
    const sourceText = 'export const marker = "plain";\n';
    const compilerOptions = { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 };
    const factory = makeCountThen();
    const again = makeCountThen();
    const instrumentedTag = transformerCacheTag({ before: [factory] });
    assert.equal(transformerCacheTag(undefined), 'none');
    assert.equal(transformerCacheTag({}), 'none');
    assert.equal(instrumentedTag, transformerCacheTag({ before: [again] }));
    assert.notEqual(factory, again);
    assert.notEqual(instrumentedTag, 'none');
    assert.notEqual(instrumentedTag, transformerCacheTag({ after: [factory] }));

    const plainKey = fixtureEmitCacheKey({ sourceText, compilerOptions, transformerTag: 'none' });
    const instrumentedKey = fixtureEmitCacheKey({
        sourceText,
        compilerOptions,
        transformerTag: instrumentedTag
    });
    assert.notEqual(plainKey, instrumentedKey);
    assert.equal(
        plainKey,
        fixtureEmitCacheKey({
            sourceText,
            compilerOptions: { target: compilerOptions.target, module: compilerOptions.module },
            transformerTag: 'none'
        })
    );
    assert.notEqual(
        plainKey,
        fixtureEmitCacheKey({
            sourceText,
            compilerOptions: { ...compilerOptions, target: ts.ScriptTarget.ES2020 },
            transformerTag: 'none'
        })
    );
    assert.notEqual(
        plainKey,
        fixtureEmitCacheKey({
            sourceText: `${sourceText}\n`,
            compilerOptions,
            transformerTag: 'none'
        })
    );
    assert.notEqual(
        fixtureEmitCacheFile(sourceText, undefined),
        fixtureEmitCacheFile(sourceText, { before: [factory] })
    );
});

test('second emit copies, and instrumented output stays on its own key', async (t) => {
    const sourceText = `// ${process.pid}-${Date.now()}\nexport const marker = 'plain';\n`;
    const factory = makeCountThen();
    const outputDir = await mkdtemp(join(tmpdir(), 'moyo-fixture-emit-test-'));
    const plainCache = fixtureEmitCacheFile(sourceText, undefined);
    const instrumentedCache = fixtureEmitCacheFile(sourceText, { before: [factory] });
    const broken = 'export const = ;\n';
    const brokenCache = fixtureEmitCacheFile(broken, undefined);
    t.after(async () => {
        await rm(outputDir, { recursive: true, force: true });
        await rm(plainCache, { force: true });
        await rm(instrumentedCache, { force: true });
        await rm(brokenCache, { force: true });
    });
    await rm(plainCache, { force: true });
    await rm(instrumentedCache, { force: true });
    await rm(brokenCache, { force: true });
    resetFixtureEmitStats();

    await emitTranspiledModule({
        name: 'plain',
        sourceText,
        outputDir,
        sourceRoot: outputDir,
        reportErrors: 'throw'
    });
    await emitTranspiledModule({
        name: 'plain-copy',
        sourceText,
        outputDir,
        sourceRoot: outputDir,
        reportErrors: 'assert'
    });
    const afterPlain = fixtureEmitStats();
    assert.equal(afterPlain.transpiled, 1);
    assert.equal(afterPlain.copied, 1);
    const plainText = await readFile(join(outputDir, 'plain.mjs'), 'utf8');
    assert.equal(plainText, await readFile(join(outputDir, 'plain-copy.mjs'), 'utf8'));
    assert.match(plainText, /plain/);
    assert.doesNotMatch(plainText, /instrumented/);

    await emitTranspiledModule({
        name: 'instrumented',
        sourceText,
        outputDir,
        sourceRoot: outputDir,
        reportErrors: 'throw',
        transformers: { before: [factory] }
    });
    const instrumentedText = await readFile(join(outputDir, 'instrumented.mjs'), 'utf8');
    assert.match(instrumentedText, /instrumented/);
    assert.notEqual(instrumentedText, plainText);
    await emitTranspiledModule({
        name: 'instrumented-copy',
        sourceText,
        outputDir,
        sourceRoot: outputDir,
        reportErrors: 'throw',
        transformers: { before: [makeCountThen()] }
    });
    assert.equal(instrumentedText, await readFile(join(outputDir, 'instrumented-copy.mjs'), 'utf8'));

    await emitTranspiledModule({
        name: 'plain-again',
        sourceText,
        outputDir,
        sourceRoot: outputDir,
        reportErrors: 'throw'
    });
    assert.equal(await readFile(join(outputDir, 'plain-again.mjs'), 'utf8'), plainText);
    const afterSplit = fixtureEmitStats();
    assert.equal(afterSplit.transpiled, 2);
    assert.equal(afterSplit.copied, 3);

    await assert.rejects(
        () =>
            emitTranspiledModule({
                name: 'plain',
                sourceText,
                outputDir,
                sourceRoot: outputDir,
                reportErrors: 'nope'
            }),
        /unsupported diagnostic report nope/
    );
    assert.equal(await readFile(join(outputDir, 'plain.mjs'), 'utf8'), plainText);

    await assert.rejects(
        () =>
            emitTranspiledModule({
                name: 'broken-throw',
                sourceText: broken,
                outputDir,
                sourceRoot: outputDir,
                reportErrors: 'throw'
            }),
        (error) => error instanceof Error && error.name === 'Error'
    );
    await assert.rejects(
        () =>
            emitTranspiledModule({
                name: 'broken-assert',
                sourceText: broken,
                outputDir,
                sourceRoot: outputDir,
                reportErrors: 'assert'
            }),
        (error) => error instanceof Error && error.name === 'AssertionError'
    );
    await assert.rejects(
        () => stat(brokenCache),
        (error) => error?.code === 'ENOENT'
    );
    const afterErrors = fixtureEmitStats();
    assert.equal(afterErrors.transpiled, 5);
    assert.equal(afterErrors.copied, 3);
});
