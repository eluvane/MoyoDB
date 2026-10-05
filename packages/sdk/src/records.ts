import {
    encodeCompoundKeyParts,
    encodeIndexScalar,
    jsonDecode,
    jsonEncode,
    splitCompoundKey,
    type CompoundKeyPart
} from './codec';
import { TransactionClosedError } from './errors';
import type { BatchOp, CreateStoreOptions, DB, PutOptions, Range, Transaction, TxMode } from './types';

export interface RecordCodec<T> {
    encode(value: T): Uint8Array;
    decode(bytes: Uint8Array): T;
}

export interface RecordCodecs<K, V> {
    key: RecordCodec<K>;
    value: RecordCodec<V>;
}

export interface RecordRange<K> {
    gt?: K;
    gte?: K;
    lt?: K;
    lte?: K;
    reverse?: boolean;
    limit?: number;
}

export interface RecordItem<K, V> {
    key: K;
    value: V;
}

export type RecordBatchOp<K, V> = { kind: 'put'; key: K; value: V } | { kind: 'delete'; key: K };
export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };

const fatalDecoder = new TextDecoder('utf-8', { fatal: true });
const NUMBER_MASK = 0xffffffffffffffffn;
const NUMBER_SIGN = 1n << 63n;

function sameBytes(left: Uint8Array, right: Uint8Array): boolean {
    return left.length === right.length && left.every((byte, index) => byte === right[index]);
}

function decodeScalarKey(bytes: Uint8Array): CompoundKeyPart {
    if (!(bytes instanceof Uint8Array) || bytes.length === 0) {
        throw new TypeError('invalid scalar key encoding');
    }
    let value: CompoundKeyPart;
    switch (bytes[0]) {
        case 0x10:
            value = null;
            break;
        case 0x20:
            value = false;
            break;
        case 0x21:
            value = true;
            break;
        case 0x30: {
            if (bytes.length !== 9) {
                throw new TypeError('invalid number key encoding');
            }
            const view = new DataView(bytes.buffer, bytes.byteOffset + 1, 8);
            const ordered = view.getBigUint64(0, false);
            const bits = (ordered & NUMBER_SIGN) !== 0n ? ordered ^ NUMBER_SIGN : ordered ^ NUMBER_MASK;
            const buffer = new ArrayBuffer(8);
            const decoded = new DataView(buffer);
            decoded.setBigUint64(0, bits, false);
            value = decoded.getFloat64(0, false);
            break;
        }
        case 0x40:
            value = fatalDecoder.decode(bytes.subarray(1));
            break;
        case 0x50:
            value = bytes.slice(1);
            break;
        default:
            throw new TypeError('invalid scalar key type');
    }
    if (!sameBytes(encodeIndexScalar(value), bytes)) {
        throw new TypeError('noncanonical scalar key encoding');
    }
    return value;
}

export const scalarKeyCodec: RecordCodec<CompoundKeyPart> = Object.freeze({
    encode: encodeIndexScalar,
    decode: decodeScalarKey
});

function typedKeyCodec<T extends CompoundKeyPart>(
    type: string,
    accepts: (value: CompoundKeyPart) => value is T
): RecordCodec<T> {
    function check(value: CompoundKeyPart): T {
        if (!accepts(value)) {
            throw new TypeError(`key must be ${type}`);
        }
        return value;
    }
    return Object.freeze({
        encode(value: T): Uint8Array {
            return encodeIndexScalar(check(value));
        },
        decode(bytes: Uint8Array): T {
            return check(decodeScalarKey(bytes));
        }
    });
}

export const keyCodecs = Object.freeze({
    string: typedKeyCodec('a string', (value): value is string => typeof value === 'string'),
    number: typedKeyCodec('a finite number', (value): value is number => typeof value === 'number'),
    boolean: typedKeyCodec('a boolean', (value): value is boolean => typeof value === 'boolean'),
    null: typedKeyCodec('null', (value): value is null => value === null),
    bytes: typedKeyCodec('Uint8Array', (value): value is Uint8Array => value instanceof Uint8Array)
});

export function compoundKeyCodec<T extends readonly CompoundKeyPart[]>(
    ...parts: { [I in keyof T]: RecordCodec<T[I]> }
): RecordCodec<T> {
    return Object.freeze({
        encode(value: T): Uint8Array {
            if (!Array.isArray(value) || value.length !== parts.length) {
                throw new TypeError(`compound key must contain ${parts.length} parts`);
            }
            return encodeCompoundKeyParts(parts.map((codec, index) => codec.encode(value[index] as CompoundKeyPart)));
        },
        decode(bytes: Uint8Array): T {
            const encoded = splitCompoundKey(bytes);
            if (encoded.length !== parts.length) {
                throw new TypeError(`compound key must contain ${parts.length} parts`);
            }
            return parts.map((codec, index) => codec.decode(encoded[index])) as unknown as T;
        }
    });
}

function assertJsonValue(value: unknown): asserts value is JsonValue {
    const pending: Array<{ value: unknown; leave?: boolean }> = [{ value }];
    const active = new Set<object>();
    while (pending.length > 0) {
        const entry = pending.pop() as { value: unknown; leave?: boolean };
        const current = entry.value;
        if (entry.leave) {
            active.delete(current as object);
            continue;
        }
        if (current === null || typeof current === 'string' || typeof current === 'boolean') {
            continue;
        }
        if (typeof current === 'number' && Number.isFinite(current) && !Object.is(current, -0)) {
            continue;
        }
        if (typeof current !== 'object' || active.has(current)) {
            throw new TypeError('record must contain JSON values without cycles, nonfinite numbers or negative zero');
        }
        const array = Array.isArray(current);
        if (Object.getPrototypeOf(current) !== (array ? Array.prototype : Object.prototype)) {
            throw new TypeError('record objects must be plain objects or arrays');
        }
        active.add(current);
        pending.push({ value: current, leave: true });
        const keys = Reflect.ownKeys(current);
        if (array && keys.length !== current.length + 1) {
            throw new TypeError('record arrays must be dense and have no extra properties');
        }
        for (const key of keys) {
            if (array && key === 'length') {
                continue;
            }
            if (typeof key !== 'string') {
                throw new TypeError('record properties must have string keys');
            }
            if (array && (!/^(?:0|[1-9]\d*)$/.test(key) || Number(key) >= current.length)) {
                throw new TypeError('record arrays must have no extra properties');
            }
            const descriptor = Object.getOwnPropertyDescriptor(current, key);
            if (!descriptor || !descriptor.enumerable || !Object.hasOwn(descriptor, 'value')) {
                throw new TypeError('record properties must be enumerable data properties');
            }
            pending.push({ value: descriptor.value as unknown });
        }
    }
}

/** A type guard checks the application schema. Without one, only the JSON domain is checked. */
export function jsonRecordCodec<T = JsonValue>(validate?: (value: unknown) => value is T): RecordCodec<T> {
    function check(value: unknown): T {
        assertJsonValue(value);
        if (validate) {
            if (!validate(value)) {
                throw new TypeError('record does not match its schema');
            }
            assertJsonValue(value);
        }
        return value as T;
    }
    return Object.freeze({
        encode(value: T): Uint8Array {
            return jsonEncode(check(value));
        },
        decode(bytes: Uint8Array): T {
            return check(jsonDecode<unknown>(bytes));
        }
    });
}

function encodeRange<K>(codec: RecordCodec<K>, range: RecordRange<K>): Range {
    const encoded: Range = {};
    for (const bound of ['gt', 'gte', 'lt', 'lte'] as const) {
        if (Object.hasOwn(range, bound)) {
            encoded[bound] = codec.encode(range[bound] as K);
        }
    }
    if (range.reverse !== undefined) {
        encoded.reverse = range.reverse;
    }
    if (range.limit !== undefined) {
        encoded.limit = range.limit;
    }
    return encoded;
}

export async function withRecordTransaction<R>(
    db: DB,
    mode: TxMode,
    action: (transaction: Transaction) => Promise<R> | R
): Promise<R> {
    const transaction = await db.begin(mode);
    let result: R;
    try {
        result = await action(transaction);
    } catch (error) {
        try {
            await transaction.rollback();
        } catch (rollbackError) {
            if (!(rollbackError instanceof TransactionClosedError)) {
                throw new AggregateError([error, rollbackError], 'record transaction and rollback failed', {
                    cause: rollbackError
                });
            }
        }
        throw error;
    }
    // SDK completion methods close the transaction even when they fail.
    if (mode === 'readwrite') {
        await transaction.commit();
    } else {
        await transaction.rollback();
    }
    return result;
}

function useTransaction<R>(
    source: DB | Transaction,
    mode: TxMode,
    action: (transaction: Transaction) => Promise<R>
): Promise<R> {
    return 'begin' in source ? withRecordTransaction(source, mode, action) : action(source);
}

export class RecordIndex<I, K, V> {
    readonly name: string;
    #source: DB | Transaction;
    #store: string;
    #indexKey: RecordCodec<I>;
    #codecs: RecordCodecs<K, V>;

    constructor(
        source: DB | Transaction,
        store: string,
        name: string,
        indexKey: RecordCodec<I>,
        codecs: RecordCodecs<K, V>
    ) {
        this.#source = source;
        this.#store = store;
        this.name = name;
        this.#indexKey = indexKey;
        this.#codecs = codecs;
    }

    async get(key: I): Promise<V | null> {
        const encoded = this.#indexKey.encode(key);
        return useTransaction(this.#source, 'readonly', async (transaction) => {
            const value = await transaction.getByIndex(this.#store, this.name, encoded);
            return value === null ? null : this.#codecs.value.decode(value);
        });
    }

    async scan(range: RecordRange<I> = {}): Promise<Array<RecordItem<K, V>>> {
        const encoded = encodeRange(this.#indexKey, range);
        return useTransaction(this.#source, 'readonly', async (transaction) => {
            const rows: Array<RecordItem<K, V>> = [];
            for await (const [key, value] of transaction.scanByIndex(this.#store, this.name, encoded)) {
                rows.push({ key: this.#codecs.key.decode(key), value: this.#codecs.value.decode(value) });
            }
            return rows;
        });
    }
}

export class RecordStore<K, V> {
    readonly name: string;
    #source: DB | Transaction;
    #codecs: RecordCodecs<K, V>;

    constructor(source: DB | Transaction, name: string, codecs: RecordCodecs<K, V>) {
        this.#source = source;
        this.name = name;
        this.#codecs = codecs;
    }

    async get(key: K): Promise<V | null> {
        const value = await this.#source.get(this.name, this.#codecs.key.encode(key));
        return value === null ? null : this.#codecs.value.decode(value);
    }

    async getMany(keys: readonly K[]): Promise<Array<V | null>> {
        const values = await this.#source.getMany(
            this.name,
            keys.map((key) => this.#codecs.key.encode(key))
        );
        return values.map((value) => (value === null ? null : this.#codecs.value.decode(value)));
    }

    async has(key: K): Promise<boolean> {
        return this.#source.has(this.name, this.#codecs.key.encode(key));
    }

    async put(key: K, value: V, options?: PutOptions): Promise<void> {
        await this.#source.put(this.name, this.#codecs.key.encode(key), this.#codecs.value.encode(value), options);
    }

    async putMany(entries: ReadonlyArray<readonly [K, V]>, options?: PutOptions): Promise<void> {
        const encoded = entries.map(([key, value]): [Uint8Array, Uint8Array] => [
            this.#codecs.key.encode(key),
            this.#codecs.value.encode(value)
        ]);
        await useTransaction(this.#source, 'readwrite', (transaction) =>
            transaction.putMany(this.name, encoded, options)
        );
    }

    async delete(key: K): Promise<boolean> {
        return this.#source.delete(this.name, this.#codecs.key.encode(key));
    }

    async deleteMany(keys: readonly K[]): Promise<void> {
        const encoded = keys.map((key) => this.#codecs.key.encode(key));
        await useTransaction(this.#source, 'readwrite', (transaction) => transaction.deleteMany(this.name, encoded));
    }

    async applyBatch(operations: ReadonlyArray<RecordBatchOp<K, V>>): Promise<void> {
        const encoded = operations.map((operation): BatchOp => {
            const kind = (operation as { kind: unknown }).kind;
            if (kind !== 'put' && kind !== 'delete') {
                throw new TypeError('record batch operation must be put or delete');
            }
            const key = this.#codecs.key.encode(operation.key);
            if (operation.kind === 'delete') {
                return { kind: 'delete', key };
            }
            return { kind: 'put', key, value: this.#codecs.value.encode(operation.value) };
        });
        await useTransaction(this.#source, 'readwrite', (transaction) => transaction.applyBatch(this.name, encoded));
    }

    async scan(range: RecordRange<K> = {}): Promise<Array<RecordItem<K, V>>> {
        const rows = await this.#source.scan(this.name, encodeRange(this.#codecs.key, range));
        return rows.map(({ key, value }) => ({
            key: this.#codecs.key.decode(key),
            value: this.#codecs.value.decode(value)
        }));
    }

    async clear(): Promise<void> {
        await this.#source.clearStore(this.name);
    }

    index<I>(name: string, key: RecordCodec<I>): RecordIndex<I, K, V> {
        return new RecordIndex(this.#source, this.name, name, key, this.#codecs);
    }

    transaction<R>(
        mode: TxMode,
        action: (store: RecordStore<K, V>, transaction: Transaction) => Promise<R> | R
    ): Promise<R> {
        if (!('begin' in this.#source)) {
            return Promise.reject(new TypeError('record store already belongs to a transaction'));
        }
        return withRecordTransaction(this.#source, mode, (transaction) =>
            action(new RecordStore(transaction, this.name, this.#codecs), transaction)
        );
    }
}

/** Bind codecs to an existing store. The database still enforces store existence. */
export function openRecordStore<K, V>(
    source: DB | Transaction,
    name: string,
    codecs: RecordCodecs<K, V>
): RecordStore<K, V> {
    return new RecordStore(source, name, codecs);
}

export async function createRecordStore<K, V>(
    source: DB | Transaction,
    name: string,
    codecs: RecordCodecs<K, V>,
    options?: CreateStoreOptions
): Promise<RecordStore<K, V>> {
    await source.createStore(name, options);
    return openRecordStore(source, name, codecs);
}
