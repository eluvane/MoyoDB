#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { closeSync, mkdirSync, mkdtempSync, openSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { delimiter, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const proofRoot = join(root, 'proofs', 'moyodb_proofs');
const temporaryRoot = join(root, '.tmp', 'lean-linters');
mkdirSync(temporaryRoot, { recursive: true });
const work = mkdtempSync(join(temporaryRoot, 'gate-'));
const lake = process.env.MOYO_LAKE ?? (process.platform === 'win32' ? 'lake.exe' : 'lake');
const environment = { ...process.env };
let serial = 0;

function run(args, { input, expectedStatus = 0 } = {}) {
    const prefix = join(work, String(serial++));
    const output = openSync(`${prefix}.out`, 'w');
    const error = openSync(`${prefix}.err`, 'w');
    let stdin = 'inherit';
    if (input !== undefined) {
        writeFileSync(`${prefix}.in`, input);
        stdin = openSync(`${prefix}.in`, 'r');
    }
    let result;
    try {
        result = spawnSync(lake, args, {
            cwd: proofRoot,
            env: environment,
            stdio: [stdin, output, error],
            shell: false,
            timeout: 180_000
        });
    } finally {
        closeSync(output);
        closeSync(error);
        if (typeof stdin === 'number') closeSync(stdin);
    }
    const stdout = readFileSync(`${prefix}.out`, 'utf8');
    const stderr = readFileSync(`${prefix}.err`, 'utf8');
    if (result.error || result.status !== expectedStatus) {
        throw new Error(
            `lake ${args.join(' ')} failed (${result.error?.message ?? result.status}):\n${stdout}${stderr}`
        );
    }
    return { stdout, stderr };
}

function collectSources(directory) {
    return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
        if (entry.name.startsWith('.')) return [];
        const path = join(directory, entry.name);
        return entry.isDirectory() ? collectSources(path) : path.endsWith('.lean') ? [path] : [];
    });
}

function heron(paths, input) {
    // --all also adds reversible refactor actions (e.g. flipping an if). All 22
    // checks, including informational checks, are enabled without that flag.
    const { stdout, stderr } = run(['exe', 'heron-scan', '--per-file', '--json', ...paths], { input });
    if (stderr.trim()) throw new Error(`Heron could not inspect every input:\n${stderr}`);
    const findings = JSON.parse(stdout);
    if (!Array.isArray(findings)) throw new Error('Heron returned an invalid report');
    return findings;
}

function denyHeron(findings) {
    if (findings.length) {
        throw new Error(`Heron findings are errors (including information):\n${JSON.stringify(findings, null, 2)}`);
    }
}

function validateJunkReport(report, expectedFindings = false) {
    const parsed = JSON.parse(report.stdout);
    if (
        report.stderr.trim() ||
        parsed.tool !== 'junk-audit' ||
        parsed.schemaVersion !== 2 ||
        parsed.guardProfile !== 'v4.31' ||
        parsed.modulesAudited < 1 ||
        parsed.options?.defBodies !== true ||
        parsed.options.presence !== true ||
        parsed.options.discharge !== true ||
        parsed.options.defsOnly !== false ||
        !Array.isArray(parsed.findings) ||
        parsed.findings.length > 0 !== expectedFindings
    ) {
        throw new Error(`Incomplete or non-strict JunkLinter report:\n${report.stdout}${report.stderr}`);
    }
    return parsed;
}

function batteriesSource(modules, prefix) {
    const imports = modules.map((module) => `import ${module}`).join('\n');
    return `${imports}\nimport Batteries.Tactic.Lint\n\n#lint- docBlameThm checkType explicitVarsOfIff in ${prefix}\n`;
}

function selfTest() {
    const information = heron(
        ['-'],
        '/-- Addition witness. -/\ndef lintWitness (n : Nat) : Nat := (fun x => x + 1) n\n'
    );
    if (!information.some((finding) => finding.rule === 'funToCdot')) {
        throw new Error('Heron informational-rule witness was not detected');
    }
    let denied = false;
    try {
        denyHeron(information);
    } catch {
        denied = true;
    }
    if (!denied) throw new Error('Heron informational findings were allowed');
    denyHeron(heron(['-'], '/-- Addition witness. -/\ndef lintWitness (n : Nat) : Nat := n + 1\n'));

    const bad = join(work, 'JunkBad.lean');
    const good = join(work, 'JunkGood.lean');
    writeFileSync(bad, '/-- Unguarded subtraction witness. -/\ndef difference (n m : Nat) : Nat := n - m\n');
    writeFileSync(
        good,
        '/-- Guarded subtraction witness. -/\ndef difference (n m : Nat) (_h : m ≤ n) : Nat := n - m\n'
    );
    for (const [file, module, expectedStatus] of [
        [bad, 'JunkBad', 1],
        [good, 'JunkGood', 0]
    ]) {
        const olean = join(proofRoot, '.lake', 'build', 'lib', 'lean', `${module}.olean`);
        run(['env', 'lean', `--root=${work}`, '-o', olean, file]);
        const report = run(['exe', 'junk-audit', '--json', module], { expectedStatus });
        const parsed = validateJunkReport(report, expectedStatus === 1);
        if (expectedStatus === 1 && !JSON.stringify(parsed).includes('Nat.sub')) {
            throw new Error('JunkLinter rejected the witness for the wrong reason');
        }
    }

    const simpBad = join(work, 'SimpBad.lean');
    writeFileSync(
        simpBad,
        'import Batteries.Tactic.Lint\n\n/-- Commutativity witness. -/\n@[simp] theorem add_flip (a b : Nat) : a + b = b + a := Nat.add_comm a b\n\n#lint- docBlameThm checkType explicitVarsOfIff\n'
    );
    const rejected = run(['env', 'lean', simpBad], { expectedStatus: 1 });
    if (!`${rejected.stdout}${rejected.stderr}`.includes('simpComm')) {
        throw new Error('Batteries rejected the witness for the wrong reason');
    }
    process.stdout.write('Lean linter witnesses passed: informational deny, guarded subtraction, bad simp lemma.\n');
}

function main() {
    // Windows executables with supportInterpreter need the pinned Lean DLLs.
    const sysroot = run(['env', 'lean', '--print-prefix']).stdout.trim();
    const pathKey = Object.keys(environment).find((key) => key.toLowerCase() === 'path') ?? 'PATH';
    environment[pathKey] = `${join(sysroot, 'bin')}${delimiter}${environment[pathKey] ?? ''}`;
    const sources = collectSources(proofRoot).toSorted();
    const modules = sources
        .filter((path) => !path.endsWith('lakefile.lean'))
        .map((path) =>
            relative(proofRoot, path)
                .replaceAll('\\', '/')
                .replace(/\.lean$/u, '')
                .replaceAll('/', '.')
        );
    const build = run([
        'build',
        '--wfail',
        ...modules.map((module) => `+${module}`),
        'heron-scan',
        'junk-audit',
        'Batteries.Tactic.Lint'
    ]);
    process.stdout.write(build.stdout + build.stderr);
    const builtin = run(['lint', '--lint-all', '--builtin-only', ...modules]);
    process.stdout.write(builtin.stdout + builtin.stderr);
    selfTest();
    const failures = [];
    // Lean resets initializer execution after importing a file's environment.
    // A fresh process per file avoids Heron's multi-file per-file import failure.
    for (const path of sources) {
        try {
            denyHeron(heron([relative(proofRoot, path).replaceAll('\\', '/')]));
        } catch (error) {
            failures.push(error.message);
        }
    }
    process.stdout.write(`Heron: inspected ${sources.length} source files.\n`);
    // Import every module, grouping by its source root so shared proofs are not
    // repeatedly audited. Executable roots with separate `main`s stay separate.
    const groups = Map.groupBy(modules, (module) => module.split('.')[0]);
    for (const [prefix, group] of groups) {
        try {
            const junk = run(['exe', 'junk-audit', '--json', ...group]);
            validateJunkReport(junk);
        } catch (error) {
            failures.push(error.message);
        }
        const audit = join(work, 'BatteriesAudit.lean');
        writeFileSync(audit, batteriesSource(group, prefix));
        try {
            run(['env', 'lean', '-DwarningAsError=true', audit]);
        } catch (error) {
            failures.push(error.message);
        }
        process.stdout.write(`JunkLinter + all Batteries linters: inspected ${prefix} (${group.length} modules).\n`);
    }
    if (failures.length) throw new Error(failures.join('\n\n'));
    process.stdout.write('Strict Lean linter gate passed.\n');
}

try {
    main();
} catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
}
