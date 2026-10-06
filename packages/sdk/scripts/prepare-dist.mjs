import { copyFile, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import ts from 'typescript';

const dist = new URL('../dist/', import.meta.url);
for (const name of await readdir(dist)) {
    if (!name.endsWith('.d.ts')) continue;
    const path = new URL(name, dist);
    const text = await readFile(path, 'utf8');
    const source = ts.createSourceFile(name, text, ts.ScriptTarget.Latest, true);
    const specifiers = [];
    const visit = (node) => {
        if ((ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) && node.moduleSpecifier) {
            specifiers.push(node.moduleSpecifier);
        } else if (ts.isImportTypeNode(node) && ts.isLiteralTypeNode(node.argument)) {
            specifiers.push(node.argument.literal);
        }
        ts.forEachChild(node, visit);
    };
    visit(source);
    let output = text;
    // Node ESM resolves declaration imports with the same extensions as JavaScript.
    for (const specifier of specifiers.toReversed()) {
        if (
            ts.isStringLiteral(specifier) &&
            /^\.{1,2}\//u.test(specifier.text) &&
            !/\.[a-z]+$/iu.test(specifier.text)
        ) {
            const end = specifier.end - 1;
            output = output.slice(0, end) + '.js' + output.slice(end);
        }
    }
    if (output !== text) await writeFile(path, output);
}

await rm(new URL('../dist/engine/.gitignore', import.meta.url), { force: true });
for (const name of ['node-worker.mjs', 'node-storage.mjs', 'node-storage.d.mts']) {
    await copyFile(new URL(`../src/${name}`, import.meta.url), new URL(`../dist/${name}`, import.meta.url));
}
// npm packs only package files, so include the root license in the SDK package.
await copyFile(new URL('../../../LICENSE', import.meta.url), new URL('../LICENSE', import.meta.url));
await copyFile(new URL('../../../README.md', import.meta.url), new URL('../README.md', import.meta.url));
