import { readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { Buffer } from 'node:buffer';
import process from 'node:process';
import ts from 'typescript';

const [source, output] = process.argv.slice(2);
if (!source || !output) {
    throw new Error('usage: node generate-sdk.mjs BASELINE_SDK_SOURCE OUTPUT_JSON');
}
const urls = new Map();
const modules = new Map();
for (const name of ['errors', 'internal', 'codec', 'indexing']) {
    const input = await readFile(resolve(source, `${name}.ts`), 'utf8');
    const result = ts.transpileModule(input, {
        fileName: `${name}.ts`,
        reportDiagnostics: true,
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
    });
    if ((result.diagnostics ?? []).some((diagnostic) => diagnostic.category === ts.DiagnosticCategory.Error)) {
        throw new Error(`failed to compile baseline ${name}`);
    }
    const code = result.outputText.replace(/from '(\.\/[^']+)'/g, (_match, specifier) => {
        const url = urls.get(specifier.slice(2));
        if (!url) {
            throw new Error(`unknown baseline dependency ${specifier}`);
        }
        return `from '${url}'`;
    });
    const url = `data:text/javascript;base64,${Buffer.from(code).toString('base64')}`;
    urls.set(name, url);
    modules.set(name, await import(url));
}
const codec = modules.get('codec');
const indexing = modules.get('indexing');
const errors = modules.get('errors');
const hex = (bytes) => Buffer.from(bytes).toString('hex');
const parts = [null, false, true, -10, -0, 1.5, '', 'a\u0000я', new Uint8Array([0, 255])];
const definitions = indexing.normalizeIndexDefinitions([
    { store: 'people', name: 'by-email', keyPath: 'email', unique: true },
    { store: 'people', name: 'by-city-age', keyPath: ['address.city', 'age'] }
]);
const indexInput = await readFile(resolve(source, 'index.ts'), 'utf8');
const mainExports = Array.from(indexInput.matchAll(/^export (?:async )?function (\w+)/gm), (match) => match[1]);
const golden = {
    release: '1.0.1',
    commit: '33ddfbfbf72f74443ce698010f083c78aa9931ea',
    exports: [...mainExports, ...Object.keys(codec), ...Object.keys(indexing), ...Object.keys(errors)].sort(),
    errors: Object.entries(errors)
        .filter(([name, ErrorConstructor]) => name !== 'MoyoDbError' && ErrorConstructor.prototype instanceof Error)
        .map(([name, ErrorConstructor]) => {
            const error = new ErrorConstructor('compatibility');
            return { export: name, name: error.name, code: error.code };
        }),
    scalarKeys: parts.map((part) => hex(codec.indexKey(part))),
    compoundKey: hex(codec.compoundKey(...parts)),
    rawCompound: hex(codec.encodeCompoundKeyParts([new Uint8Array([0, 255]), new Uint8Array([])])),
    u64Keys: [0n, 1n, 4294967296n, 18446744073709551615n].map((value) => hex(codec.u64Key(value))),
    json: hex(codec.jsonEncode({ label: 'я\u0000', nested: [null, true, 7] })),
    metadataStore: indexing.INDEX_METADATA_STORE,
    indexMetadata: definitions.map((def) => ({
        public: indexing.toPublicIndexDefinitions([def])[0],
        internalStore: def.internalStore,
        key: hex(indexing.encodeIndexMetadataKey(def.store, def.name)),
        value: hex(indexing.encodeIndexMetadataValue(def)),
        logicalKey: hex(
            indexing.extractLogicalIndexKey(
                def,
                codec.jsonEncode({ email: 'a@example.test', address: { city: 'Москва' }, age: 42 })
            )
        )
    })),
    indexEntry: hex(indexing.encodeIndexEntryKey(codec.indexKey('a\u0000я'), new Uint8Array([0, 1, 255])))
};
await writeFile(resolve(output), `${JSON.stringify(golden, null, 2)}\n`);
