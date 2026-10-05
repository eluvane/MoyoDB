#!/usr/bin/env node
import { spawnSync } from 'node:child_process';

// Criterion benches run natively; the production library also needs its
// wasm-only bindings and storage paths checked by Clippy.
const targets = [['--all-targets'], ['--lib', '--target', 'wasm32-unknown-unknown']];
const cargo = process.platform === 'win32' ? 'cargo.exe' : 'cargo';
for (const target of targets) {
    const result = spawnSync(cargo, ['clippy', '--locked', '--workspace', ...target, '--', '-D', 'warnings'], {
        cwd: process.cwd(),
        env: {
            ...process.env,
            CLIPPY_CONF_DIR: '.config/moyo/build'
        },
        shell: false,
        stdio: 'inherit'
    });
    if (result.error) throw result.error;
    if (result.status !== 0) {
        process.exitCode = result.status ?? 1;
        break;
    }
}
