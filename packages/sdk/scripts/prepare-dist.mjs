import { copyFile, rm } from 'node:fs/promises';

await rm(new URL('../dist/engine/.gitignore', import.meta.url), { force: true });
// npm only packs files inside the package directory, so the published SDK carries a copy of the root license.
await copyFile(new URL('../../../LICENSE', import.meta.url), new URL('../LICENSE', import.meta.url));
