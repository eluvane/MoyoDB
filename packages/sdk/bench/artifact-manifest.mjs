import { createHash } from 'node:crypto';
import fs from 'node:fs/promises';
import path from 'node:path';

export function hash(bytes) {
    return createHash('sha256').update(bytes).digest('hex');
}

export async function artifactManifest(directory, prefix = '') {
    const entries = [];
    for (const entry of await fs.readdir(directory, { withFileTypes: true })) {
        const filename = path.join(directory, entry.name);
        const relative = prefix + entry.name;
        if (entry.isDirectory()) {
            entries.push(...Object.entries(await artifactManifest(filename, relative + '/')));
        } else if (entry.isFile()) {
            const bytes = await fs.readFile(filename);
            entries.push([relative, { sha256: hash(bytes), bytes: bytes.length }]);
        } else {
            throw Error(`unsupported engine artifact entry: ${filename}`);
        }
    }
    return Object.fromEntries(entries.sort(([a], [b]) => a.localeCompare(b)));
}
