#!/usr/bin/env node
import { spawnSync } from 'node:child_process';

const result = spawnSync('cargo', ['clippy', '--workspace', '--all-targets', '--', '-D', 'warnings'], {
    cwd: process.cwd(),
    env: {
        ...process.env,
        CLIPPY_CONF_DIR: '.config/moyo/build'
    },
    shell: process.platform === 'win32',
    stdio: 'inherit'
});

process.exit(result.status ?? 1);
