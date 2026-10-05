import { copyFile, rm } from 'node:fs/promises';

await rm(new URL('../dist/engine/.gitignore', import.meta.url), { force: true });
// npm packs only package files, so include the root license in the SDK package.
await copyFile(new URL('../../../LICENSE', import.meta.url), new URL('../LICENSE', import.meta.url));
