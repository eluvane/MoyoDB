import type * as Records from '../src/records';
import type * as Codec from '../src/codec';
import type { DB, Transaction } from '../src/types';

function ok(value: unknown, message: string): asserts value {
    if (!value) {
        throw new Error(message);
    }
}

function equalBytes(left: Uint8Array, right: Uint8Array): void {
    ok(left.length === right.length && left.every((byte, index) => byte === right[index]), 'encoded bytes differ');
}

function compareBytes(left: Uint8Array, right: Uint8Array): number {
    for (let index = 0; index < Math.min(left.length, right.length); index += 1) {
        if (left[index] !== right[index]) {
            return left[index] - right[index];
        }
    }
    return left.length - right.length;
}

async function rejects(run: () => unknown, expected = 'TypeError'): Promise<Error> {
    try {
        await run();
    } catch (error) {
        ok(error instanceof Error && error.name === expected, `expected ${expected}, got ${String(error)}`);
        return error;
    }
    throw new Error(`expected ${expected}`);
}

export async function checkRecordWork(records: typeof Records, codec: typeof Codec) {
    const results: Array<{ name: string; passed: boolean; error?: string }> = [];
    async function test(name: string, run: () => unknown) {
        try {
            await run();
            results.push({ name, passed: true });
        } catch (error) {
            results.push({ name, passed: false, error: error instanceof Error ? error.message : String(error) });
        }
    }

    await test('scalar keys round trip without changing existing persisted bytes', () => {
        const values = [null, false, true, -42.25, 0, 3, '', 'a\u0000😀', Uint8Array.of(0, 255)];
        for (const value of values) {
            const encoded = records.scalarKeyCodec.encode(value);
            equalBytes(encoded, codec.indexKey(value));
            const decoded = records.scalarKeyCodec.decode(encoded);
            if (value instanceof Uint8Array) {
                ok(decoded instanceof Uint8Array, 'byte key type');
                equalBytes(decoded, value);
            } else {
                ok(Object.is(decoded, value), 'scalar key value');
            }
        }
        for (let index = 1; index < values.length; index += 1) {
            ok(
                compareBytes(
                    records.scalarKeyCodec.encode(values[index - 1]),
                    records.scalarKeyCodec.encode(values[index])
                ) < 0,
                'mixed scalar order'
            );
        }
    });
    await test('number keys preserve finite order and normalize negative zero', () => {
        const values = [-Number.MAX_VALUE, -10, -Number.MIN_VALUE, 0, Number.MIN_VALUE, 10, Number.MAX_VALUE];
        for (let index = 0; index < values.length; index += 1) {
            const encoded = records.keyCodecs.number.encode(values[index]);
            ok(records.keyCodecs.number.decode(encoded) === values[index], 'number round trip');
            if (index > 0) {
                ok(compareBytes(records.keyCodecs.number.encode(values[index - 1]), encoded) < 0, 'numeric order');
            }
        }
        ok(Object.is(records.keyCodecs.number.decode(records.keyCodecs.number.encode(-0)), 0), 'negative zero');
    });
    await test('string keys preserve Unicode scalar order including embedded zero', () => {
        const values = ['', '\u0000', '\u0000a', 'a', 'aa', 'é', '\ue000', '\ufffd', '😀'];
        for (let index = 0; index < values.length; index += 1) {
            const encoded = records.keyCodecs.string.encode(values[index]);
            ok(records.keyCodecs.string.decode(encoded) === values[index], 'string round trip');
            if (index > 0) {
                ok(compareBytes(records.keyCodecs.string.encode(values[index - 1]), encoded) < 0, 'string order');
            }
        }
    });
    await test('byte key buffers remain owned after encoding and decoding', () => {
        const input = Uint8Array.of(0, 255, 1);
        const encoded = records.keyCodecs.bytes.encode(input);
        input.fill(2);
        const decoded = records.keyCodecs.bytes.decode(encoded);
        equalBytes(decoded, Uint8Array.of(0, 255, 1));
        decoded.fill(3);
        equalBytes(records.keyCodecs.bytes.decode(encoded), Uint8Array.of(0, 255, 1));
        ok(
            compareBytes(
                records.keyCodecs.bytes.encode(Uint8Array.of(0)),
                records.keyCodecs.bytes.encode(Uint8Array.of(0, 1))
            ) < 0,
            'byte prefix order'
        );
        ok(
            compareBytes(
                records.keyCodecs.bytes.encode(Uint8Array.of(0, 255)),
                records.keyCodecs.bytes.encode(Uint8Array.of(1))
            ) < 0,
            'unsigned byte order'
        );
    });
    await test('typed compound keys preserve tuple order and existing escape encoding', () => {
        const keys = records.compoundKeyCodec(
            records.keyCodecs.string,
            records.keyCodecs.number,
            records.keyCodecs.bytes
        );
        const input: [string, number, Uint8Array] = ['org\u0000', 42, Uint8Array.of(0, 255)];
        const encoded = keys.encode(input);
        equalBytes(encoded, codec.compoundKey(...input));
        const decoded = keys.decode(encoded);
        ok(decoded[0] === input[0] && decoded[1] === input[1], 'tuple values');
        equalBytes(decoded[2], input[2]);
        ok(
            compareBytes(keys.encode(['org', 1, new Uint8Array()]), keys.encode(['org', 2, new Uint8Array()])) < 0,
            'tuple numeric order'
        );
        ok(
            compareBytes(keys.encode(['org', 1, new Uint8Array()]), keys.encode(['orgA', -1, new Uint8Array()])) < 0,
            'tuple string prefix order'
        );
    });
    await test('typed keys reject wrong domains and malformed compound shapes', async () => {
        for (const value of [NaN, Infinity, -Infinity, undefined, {}, 1n]) {
            await rejects(() => records.scalarKeyCodec.encode(value as never));
        }
        await rejects(() => records.keyCodecs.number.encode('12' as never));
        await rejects(() => records.keyCodecs.string.decode(codec.indexKey(12)));
        await rejects(() => records.keyCodecs.string.encode('\ud800'));
        const tuple = records.compoundKeyCodec(records.keyCodecs.string, records.keyCodecs.number);
        await rejects(() => tuple.encode(['a'] as never));
        await rejects(() => tuple.encode(['a', '1'] as never));
        await rejects(() => tuple.decode(codec.compoundKey('a')));
        await rejects(() => tuple.decode(Uint8Array.of(0, 1)));
    });
    await test('scalar decode rejects malformed, invalid Unicode and noncanonical numbers', async () => {
        for (const bytes of [
            new Uint8Array(),
            Uint8Array.of(0x01),
            Uint8Array.of(0x10, 0),
            Uint8Array.of(0x20, 0),
            Uint8Array.of(0x30),
            Uint8Array.of(0x40, 255),
            Uint8Array.of(0x30, 0xff, 0xf0, 0, 0, 0, 0, 0, 0),
            Uint8Array.of(0x30, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff)
        ]) {
            await rejects(() => records.scalarKeyCodec.decode(bytes));
        }
    });
    await test('JSON records round trip nested data using unchanged JSON bytes', () => {
        const values = [null, true, 12.5, '\ud800', { name: 'Ada', tags: ['a', null], nested: { enabled: true } }];
        const json = records.jsonRecordCodec();
        for (const value of values) {
            const encoded = json.encode(value);
            equalBytes(encoded, codec.jsonEncode(value));
            ok(JSON.stringify(json.decode(encoded)) === JSON.stringify(value), 'JSON round trip');
        }
        const special = JSON.parse('{"__proto__":{"name":"own"},"constructor":"own"}') as Records.JsonValue;
        ok(JSON.stringify(json.decode(json.encode(special))) === JSON.stringify(special), 'own special field');
    });
    await test('JSON records reject values that JSON would drop or change', async () => {
        const json = records.jsonRecordCodec<unknown>();
        const cycle: { self?: unknown } = {};
        cycle.self = cycle;
        for (const value of [
            undefined,
            NaN,
            Infinity,
            -Infinity,
            -0,
            1n,
            Symbol('x'),
            () => 1,
            { missing: undefined },
            [undefined],
            cycle
        ]) {
            await rejects(() => json.encode(value));
        }
        for (const value of [new Date(), new Map(), new Set(), new Uint8Array(), Object.create(null) as unknown]) {
            await rejects(() => json.encode(value));
        }
        await rejects(() => json.decode(codec.utf8Encode('{"n":1e400}')));
        await rejects(() => json.decode(codec.utf8Encode('-0')));
        await rejects(() => json.decode(Uint8Array.of(34, 255, 34)));
    });
    await test('JSON records reject sparse arrays, extra properties, symbols and accessors', async () => {
        const json = records.jsonRecordCodec<unknown>();
        const extra = Object.assign([1], { label: 'lost' });
        const holeWithExtra = Object.assign(new Array<unknown>(1), { label: 'lost' });
        let calls = 0;
        const accessor = Object.defineProperty({}, 'x', {
            enumerable: true,
            get() {
                calls += 1;
                return 1;
            }
        });
        const hidden = Object.defineProperty({}, 'x', { value: 1 });
        for (const value of [new Array<unknown>(1), extra, holeWithExtra, { [Symbol('x')]: 1 }, accessor, hidden]) {
            await rejects(() => json.encode(value));
        }
        ok(calls === 0, 'record validation invoked a getter');
    });
    await test('record schemas validate both written and existing decoded records', async () => {
        type User = { name: string; active: boolean };
        function isUser(value: unknown): value is User {
            return (
                typeof value === 'object' &&
                value !== null &&
                'name' in value &&
                typeof value.name === 'string' &&
                'active' in value &&
                typeof value.active === 'boolean'
            );
        }
        const json = records.jsonRecordCodec(isUser);
        const decoded: User = json.decode(json.encode({ name: 'Ada', active: true }));
        ok(decoded.name === 'Ada', 'typed schema round trip');
        await rejects(() => json.encode({ name: 12, active: true } as never));
        await rejects(() => json.decode(codec.jsonEncode({ name: 'Ada' })));
        const mutating = records.jsonRecordCodec((value: unknown): value is { name?: string } => {
            if (typeof value === 'object' && value !== null) {
                Object.defineProperty(value, 'name', { enumerable: true, value: undefined });
            }
            return true;
        });
        await rejects(() => mutating.encode({ name: 'Ada' }));
    });
    await test('automatic transactions return results and commit only successful callbacks', async () => {
        const calls: string[] = [];
        const transaction = {
            mode: 'readwrite',
            commit: () => {
                calls.push('commit');
                return Promise.resolve();
            },
            rollback: () => {
                calls.push('rollback');
                return Promise.resolve();
            }
        } as unknown as Transaction;
        const readonlyTransaction = {
            mode: 'readonly',
            commit: () => Promise.reject(new Error('readonly transaction cannot commit')),
            rollback: () => {
                calls.push('rollback');
                return Promise.resolve();
            }
        } as unknown as Transaction;
        const db = {
            begin: (mode: string) => {
                calls.push(mode);
                return Promise.resolve(mode === 'readonly' ? readonlyTransaction : transaction);
            }
        } as unknown as DB;
        const result = await records.withRecordTransaction(db, 'readwrite', (tx) => {
            ok(tx === transaction, 'transaction binding');
            return 42;
        });
        ok(result === 42 && calls.join(',') === 'readwrite,commit', 'commit sequence');
        calls.length = 0;
        const readonlyResult = await records.withRecordTransaction(db, 'readonly', (tx) => {
            ok(tx === readonlyTransaction, 'readonly transaction binding');
            return 'read result';
        });
        ok(readonlyResult === 'read result' && calls.join(',') === 'readonly,rollback', 'readonly completion sequence');
        calls.length = 0;
        const failure = new Error('callback failed');
        const error = await rejects(
            () =>
                records.withRecordTransaction(db, 'readwrite', () => {
                    throw failure;
                }),
            'Error'
        );
        ok(error === failure && calls.join(',') === 'readwrite,rollback', 'rollback sequence');
    });
    await test('transaction failures preserve callback and rollback errors and terminal commit failure', async () => {
        const failure = new Error('callback failed');
        const rollbackFailure = new Error('rollback failed');
        let rollbackCalls = 0;
        const transaction = {
            commit: () => {
                return Promise.reject(new Error('commit failed'));
            },
            rollback: () => {
                rollbackCalls += 1;
                return Promise.reject(rollbackFailure);
            }
        } as unknown as Transaction;
        const db = { begin: () => Promise.resolve(transaction) } as unknown as DB;
        const error = await rejects(
            () =>
                records.withRecordTransaction(db, 'readwrite', () => {
                    throw failure;
                }),
            'AggregateError'
        );
        ok(
            error instanceof AggregateError && error.errors[0] === failure && error.errors[1] === rollbackFailure,
            'failure details'
        );
        rollbackCalls = 0;
        await rejects(() => records.withRecordTransaction(db, 'readwrite', () => 1), 'Error');
        ok(rollbackCalls === 0, 'commit is terminal in the SDK');
    });
    await test('bulk encoding validates every record before opening a transaction or writing', async () => {
        let beginCalls = 0;
        const db = {
            begin: () => {
                beginCalls += 1;
                return Promise.reject(new Error('unexpected transaction'));
            }
        } as unknown as DB;
        const store = records.openRecordStore(db, 'users', {
            key: records.keyCodecs.number,
            value: records.jsonRecordCodec<unknown>()
        });
        await rejects(() =>
            store.putMany([
                [1, { name: 'Ada' }],
                [2, { value: undefined }]
            ])
        );
        await rejects(() => store.deleteMany([1, NaN]));
        await rejects(() =>
            store.applyBatch([
                { kind: 'put', key: 1, value: {} },
                { kind: 'put', key: 2, value: undefined }
            ])
        );
        await rejects(() => store.applyBatch([{ kind: 'append', key: 1, value: {} }] as never));
        ok(beginCalls === 0, 'invalid bulk encoding opened a transaction');
    });
    return {
        passed: results.filter((result) => result.passed).length,
        failed: results.filter((result) => !result.passed).length,
        results
    };
}
