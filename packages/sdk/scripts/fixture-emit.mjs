import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { copyFile, mkdir, mkdtemp, readFile, rename, rm, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const CACHE_VERSION = 1;
const TRANSFORMER_PHASES = ['before', 'after', 'afterDeclarations'];

const emitCounts = {
    transpiled: 0,
    copied: 0
};

export function readFlagValue(args, flag) {
    const index = args.indexOf(flag);
    if (index < 0) return undefined;
    if (!args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`missing ${flag} value`);
    return args[index + 1];
}

export function fixtureEmitStats() {
    return { ...emitCounts };
}

export function resetFixtureEmitStats() {
    emitCounts.transpiled = 0;
    emitCounts.copied = 0;
}

export function emitCompilerOptions() {
    return { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext };
}

function canonicalJson(value) {
    if (Array.isArray(value)) return `[${value.map((item) => canonicalJson(item)).join(',')}]`;
    if (value && typeof value === 'object') {
        const keys = Object.keys(value)
            .filter((key) => value[key] !== undefined)
            .sort();
        return `{${keys.map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(',')}}`;
    }
    return JSON.stringify(value) ?? 'null';
}

// Function source, not object identity: a new process loads the same factory text.
export function transformerCacheTag(transformers) {
    if (transformers === null || transformers === undefined) return 'none';
    const parts = [];
    for (const phase of TRANSFORMER_PHASES) {
        const factories = transformers[phase];
        if (factories === null || factories === undefined) continue;
        if (!Array.isArray(factories)) throw new Error(`unsupported transformer list ${phase}`);
        for (let index = 0; index < factories.length; index += 1) {
            const factory = factories[index];
            if (typeof factory !== 'function') throw new Error(`unsupported transformer ${phase}[${index}]`);
            parts.push(`${phase}:${index}:${Function.prototype.toString.call(factory)}`);
        }
    }
    if (parts.length === 0) return 'none';
    return createHash('sha256').update(parts.join('\n')).digest('hex');
}

export function fixtureEmitCacheKey({ sourceText, compilerOptions, transformerTag }) {
    return createHash('sha256')
        .update(String(CACHE_VERSION))
        .update('\0')
        .update(ts.version)
        .update('\0')
        .update(canonicalJson(compilerOptions))
        .update('\0')
        .update(String(transformerTag))
        .update('\0')
        .update(sourceText)
        .digest('hex');
}

export function fixtureEmitCacheDir() {
    return fileURLToPath(new URL('../../../.tmp/fixture-emit/', import.meta.url));
}

export function fixtureEmitCacheFile(sourceText, transformers) {
    const key = fixtureEmitCacheKey({
        sourceText,
        compilerOptions: emitCompilerOptions(),
        transformerTag: transformerCacheTag(transformers)
    });
    return join(fixtureEmitCacheDir(), key);
}

async function copyCachedEmit(cacheFile, destination) {
    try {
        await stat(cacheFile);
    } catch (error) {
        if (error?.code === 'ENOENT') return false;
        throw error;
    }
    await copyFile(cacheFile, destination);
    return true;
}

async function publishCachedEmit(cacheFile, emitted) {
    await mkdir(fixtureEmitCacheDir(), { recursive: true });
    const directory = await mkdtemp(join(fixtureEmitCacheDir(), 'publish-'));
    const temporary = join(directory, 'emitted');
    try {
        await writeFile(temporary, emitted, { flag: 'wx', mode: 0o600 });
        try {
            await rename(temporary, cacheFile);
        } catch (error) {
            if (error?.code !== 'EEXIST' && error?.code !== 'EPERM') throw error;
        }
    } finally {
        await rm(directory, { recursive: true, force: true });
    }
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
    const cacheableReport = reportErrors === 'assert' || reportErrors === 'throw';
    const destination = join(outputDir, `${name}.mjs`);
    const cacheFile = cacheableReport ? fixtureEmitCacheFile(text, transformers) : undefined;
    if (cacheFile && (await copyCachedEmit(cacheFile, destination))) {
        emitCounts.copied += 1;
        return;
    }
    const transpileOptions = {
        fileName: `${name}.ts`,
        reportDiagnostics: true,
        compilerOptions: emitCompilerOptions()
    };
    if (transformers !== undefined) transpileOptions.transformers = transformers;
    const result = ts.transpileModule(text, transpileOptions);
    emitCounts.transpiled += 1;
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
    const emitted = result.outputText.replace(/from '(\.\/[^']+)'/g, "from '$1.mjs'");
    await writeFile(destination, emitted);
    if (cacheFile) await publishCachedEmit(cacheFile, emitted);
}
