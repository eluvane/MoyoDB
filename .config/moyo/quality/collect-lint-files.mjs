import { readdirSync } from 'node:fs';
import { join } from 'node:path';
import ignore from 'ignore';

export function collectLintFiles(select, patterns) {
    const excluded = ignore().add(['.git/', ...patterns]);
    const files = [];
    function visit(directory, relativeDirectory) {
        for (const entry of readdirSync(directory, { withFileTypes: true })) {
            const relative = relativeDirectory ? `${relativeDirectory}/${entry.name}` : entry.name;
            if (entry.isDirectory()) {
                if (!excluded.ignores(`${relative}/`)) visit(join(directory, entry.name), relative);
            } else if (entry.isFile() && !excluded.ignores(relative) && select(relative)) {
                files.push(relative);
            }
        }
    }
    visit('.', '');
    return files.sort();
}
