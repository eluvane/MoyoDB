import { readFileSync } from 'node:fs';
import ignore from 'ignore';
import { lint } from 'markdownlint/promise';
import { collectLintFiles } from './collect-lint-files.mjs';

const settings = JSON.parse(readFileSync('.config/moyo/lints/markdownlint-cli2.jsonc', 'utf8'));
const included = ignore().add(settings.globs.filter((pattern) => !pattern.startsWith('!')));
const excluded = settings.globs.filter((pattern) => pattern.startsWith('!')).map((pattern) => pattern.slice(1));
const files = collectLintFiles((file) => included.ignores(file), excluded);
if (files.length === 0) throw new Error('Markdown lint scope contains no files.');
const result = await lint({ files, config: settings.config });
const failures = Object.values(result).reduce((count, issues) => count + issues.length, 0);
if (failures > 0) {
    for (const [file, issues] of Object.entries(result)) {
        for (const issue of issues) {
            console.error(`${file}:${issue.lineNumber}: ${issue.ruleNames.join('/')} ${issue.ruleDescription}`);
        }
    }
    process.exitCode = 1;
} else {
    console.log(`Markdown lint passed: ${files.length} files checked.`);
}
