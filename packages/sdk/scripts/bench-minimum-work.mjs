import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir, cpus, platform, arch } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';
import { parseArgs } from 'node:util';
// JavaScript-layer benchmark with an ordered fake WASM backend. This deliberately
// does not report engine, OPFS, Worker transport or IndexedDB performance.
const { values } = parseArgs({ options: {
        before: { type: 'string' }, after: { type: 'string' },
        samples: { type: 'string', default: '7' }, warmups: { type: 'string', default: '2' }
    } });
if (!values.before)
    throw new Error('--before must name the baseline SDK src directory');
const samples = Number(values.samples), warmups = Number(values.warmups);
if (!Number.isSafeInteger(samples) || samples < 1 || !Number.isSafeInteger(warmups) || warmups < 0) {
    throw new Error('--samples must be a positive integer; --warmups a non-negative integer');
}
const here = dirname(fileURLToPath(import.meta.url));
const roots = { before: resolve(values.before), after: resolve(values.after ?? join(here, '../src')) };
const output = await mkdtemp(join(tmpdir(), 'moyo-work-benchmark-'));
const suites = {};
const median = (xs) => {
    const sorted = [...xs].sort((a, b) => a - b), mid = Math.floor(sorted.length / 2);
    return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
};
try {
    globalThis.self = { addEventListener() { }, removeEventListener() { } };
    for (const version of ['before', 'after']) {
        const destination = join(output, version);
        const generated = spawnSync(process.execPath, [join(here, 'test-minimum-work.mjs'),
            '--source-root', roots[version], '--emit-dir', destination], { encoding: 'utf8' });
        if (generated.status !== 0)
            throw new Error(generated.error?.message ?? generated.stderr ?? 'fixture build failed');
        const { suite } = await import(pathToFileURL(join(destination, 'suite-loader.mjs')).href);
        suites[version] = suite.makeBenchmarks();
    }
    const results = [];
    for (const name of Object.keys(suites.before.cases)) {
        for (let i = 0; i < warmups; i++) {
            await suites.before.cases[name]();
            await suites.after.cases[name]();
        }
        const measurements = { before: [], after: [] };
        for (let sample = 0; sample < samples; sample++) {
            for (const version of sample % 2 ? ['after', 'before'] : ['before', 'after']) {
                const start = performance.now();
                await suites[version].cases[name]();
                measurements[version].push(performance.now() - start);
            }
        }
        results.push({ name, ...measurements,
            beforeMedianMs: median(measurements.before), afterMedianMs: median(measurements.after) });
    }
    console.log(JSON.stringify({
        scope: 'SDK JavaScript only; mock WASM, no physical IO or Worker transport',
        roots, node: process.version, platform: platform(), arch: arch(), cpu: cpus()[0]?.model,
        samples, warmups, order: 'alternating before/after', results
    }, null, 2));
}
finally {
    for (const s of Object.values(suites))
        s.close();
    await rm(output, { recursive: true, force: true });
}
