import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import ts from 'typescript';

const compile = async (name, source) => {
    const result = ts.transpileModule(source, {
        fileName: `${name}.ts`,
        reportDiagnostics: true,
        compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext }
    });
    assert.deepEqual(
        (result.diagnostics ?? []).filter((diagnostic) => diagnostic.category === ts.DiagnosticCategory.Error),
        []
    );
    return result.outputText;
};

const internalSource = await readFile(new URL('../src/internal.ts', import.meta.url), 'utf8');
const protocolText = await readFile(new URL('../src/worker-protocol.ts', import.meta.url), 'utf8');
const clientText = await readFile(new URL('../src/worker-client.ts', import.meta.url), 'utf8');
const internalUrl = `data:text/javascript;base64,${Buffer.from(await compile('internal', internalSource)).toString('base64')}`;
const protocolSource = (await compile('worker-protocol', protocolText)).replaceAll(
    "from './internal'",
    `from '${internalUrl}'`
);
const protocolUrl = `data:text/javascript;base64,${Buffer.from(protocolSource).toString('base64')}`;
const clientSource = (await compile('worker-client', clientText))
    .replaceAll("from './internal'", `from '${internalUrl}'`)
    .replaceAll("from './worker-protocol'", `from '${protocolUrl}'`);
const { WorkerProtocolClient } = await import(
    `data:text/javascript;base64,${Buffer.from(clientSource).toString('base64')}`
);
const { MAX_TIMER_DELAY_MS, clampTimerDelayMs } = await import(internalUrl);

test('timer delays above the 32-bit limit clamp instead of wrapping', () => {
    assert.equal(clampTimerDelayMs(0), 0);
    assert.equal(clampTimerDelayMs(-5), 0);
    assert.equal(clampTimerDelayMs(Number.NaN), 0);
    assert.equal(clampTimerDelayMs(25), 25);
    assert.equal(clampTimerDelayMs(MAX_TIMER_DELAY_MS + 1), MAX_TIMER_DELAY_MS);
    assert.equal(clampTimerDelayMs(Number.MAX_SAFE_INTEGER), MAX_TIMER_DELAY_MS);
});

test('an oversized ready timeout does not fail the client immediately', async () => {
    const listeners = new Map();
    const transport = {
        postMessage() {},
        addEventListener(type, listener) {
            const list = listeners.get(type) ?? [];
            list.push(listener);
            listeners.set(type, list);
        },
        removeEventListener(type, listener) {
            listeners.set(
                type,
                (listeners.get(type) ?? []).filter((entry) => entry !== listener)
            );
        }
    };
    const client = new WorkerProtocolClient(transport, { readyTimeoutMs: 3_000_000_000 });
    let failed = false;
    const pending = client.whenReady().then(
        () => 'ready',
        () => {
            failed = true;
            return 'failed';
        }
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    assert.equal(failed, false);
    for (const listener of listeners.get('message') ?? []) {
        listener({ data: { type: 'moyodb:worker-protocol:response', version: 2, id: 1, ok: false } });
    }
    assert.equal(await pending, 'failed');
    client.dispose();
});
