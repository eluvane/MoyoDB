import { rm } from 'node:fs/promises';

await rm(new URL('../dist/engine/.gitignore', import.meta.url), { force: true });
