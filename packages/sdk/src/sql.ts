import { indexKey, jsonDecode, jsonEncode, u64Key, utf8Encode } from './codec';
import { ConstraintError, SerializationError, StoreNotFoundError } from './errors';
import { normalizeIndexDefinitions, serializeNormalizedIndexDefinitions, type NormalizedIndexDef } from './indexing';
import { bindSqlParameters, parseSql, resolveSqlValue } from './sql-parser';
import type {
    SqlColumnDefinition,
    SqlExpression,
    SqlOrderBy,
    SqlStatement,
    SqlValue,
    SqlValueExpression
} from './sql-types';
import type { DB, IndexDef, Range, ScanItem, Transaction } from './types';

const CATALOG = '_moyodb_sql_catalog';
const OWNER_KEY = Uint8Array.of(0);
const OWNER = { format: 'moyodb.sql', version: 1 };
const TABLE_OWNER_KEY = Uint8Array.of(255);
const TABLE_OWNER_FIELD = '\u0000moyodb.sql.table';
const INTEGER_MIN = -(1n << 63n);
const INTEGER_MAX = (1n << 63n) - 1n;
const ROW_ID_MAX = (1n << 64n) - 1n;
const COLUMN_TYPES = new Set(['INTEGER', 'REAL', 'TEXT', 'BOOLEAN', 'BLOB']);

export type SqlRow = Record<string, SqlValue>;
export interface SqlQueryPlan {
    table: string;
    access: 'primary-key-lookup' | 'primary-key-range' | 'index-lookup' | 'index-range' | 'table-scan';
    column?: string;
    index?: string;
    sort: 'none' | 'memory';
    reason: string;
}
export interface SqlResult {
    rows: SqlRow[];
    rowsAffected: number;
    plan?: SqlQueryPlan;
}
export interface SqlClientOptions {
    indexes?: IndexDef[];
}
export interface SqlClient {
    execute(sql: string, parameters?: readonly unknown[]): Promise<SqlResult>;
    query(sql: string, parameters?: readonly unknown[]): Promise<SqlRow[]>;
    explain(sql: string, parameters?: readonly unknown[]): Promise<SqlQueryPlan>;
}
export class SqlSchemaError extends Error {
    constructor(message: string, options?: ErrorOptions) {
        super(message, options);
        this.name = 'SqlSchemaError';
    }
}
export class SqlTypeError extends TypeError {
    constructor(message: string) {
        super(message);
        this.name = 'SqlTypeError';
    }
}
interface TableSchema {
    version: 1;
    table: string;
    columns: SqlColumnDefinition[];
    nextRowId: string;
}
interface StoredRow {
    key: Uint8Array;
    row: SqlRow;
}
interface AccessPlan {
    public: SqlQueryPlan;
    key?: Uint8Array;
    range?: Range;
    empty?: boolean;
}
type Comparison = Extract<SqlExpression, { kind: 'comparison' }>;
type Truth = boolean | null;

export function createSqlClient(db: DB, options: SqlClientOptions = {}): SqlClient {
    const expectedIndexes = options.indexes === undefined ? null : normalizeIndexDefinitions(options.indexes);
    const run = async (sql: string, parameters: readonly unknown[], explain: boolean): Promise<SqlResult> => {
        const parsed = parseSql(sql);
        const bound = bindSqlParameters(parsed, parameters);
        const statement = parsed.statement;
        if (explain && !isRowStatement(statement)) {
            throw new SqlSchemaError('EXPLAIN requires SELECT, UPDATE, or DELETE');
        }
        const tx = await db.begin(statement.kind === 'select' || explain ? 'readonly' : 'readwrite');
        let finished = false;
        try {
            const indexes = normalizeIndexDefinitions((await tx.listIndexes?.()) ?? []);
            if (
                expectedIndexes &&
                serializeNormalizedIndexDefinitions(expectedIndexes) !== serializeNormalizedIndexDefinitions(indexes)
            ) {
                throw new SqlSchemaError('SQL index definitions do not match the transaction catalog');
            }
            const result = await executeStatement(tx, statement, bound, indexes, explain);
            // Finalization closes the SDK transaction even when it fails.
            finished = true;
            if (tx.mode === 'readwrite') {
                await tx.commit();
            } else {
                await tx.rollback();
            }
            return result;
        } catch (error) {
            if (!finished) {
                try {
                    await tx.rollback();
                } catch (rollbackError) {
                    throw new AggregateError([error, rollbackError], 'SQL statement and rollback failed', {
                        cause: rollbackError
                    });
                }
            }
            throw error;
        }
    };
    return {
        execute: (sql, parameters = []) => run(sql, parameters, false),
        async query(sql, parameters = []) {
            if (parseSql(sql).statement.kind !== 'select') {
                throw new SqlSchemaError('query() requires SELECT');
            }
            return (await run(sql, parameters, false)).rows;
        },
        async explain(sql, parameters = []) {
            const result = await run(sql, parameters, true);
            if (!result.plan) {
                throw new SqlSchemaError('statement has no query plan');
            }
            return result.plan;
        }
    };
}

async function executeStatement(
    tx: Transaction,
    statement: SqlStatement,
    parameters: SqlValue[],
    indexes: NormalizedIndexDef[],
    explain: boolean
): Promise<SqlResult> {
    assertTableName(statement.table);
    const catalogExists = await readCatalog(tx);
    const schema = catalogExists ? await readSchema(tx, statement.table) : null;
    if (schema) await assertTableOwnership(tx, schema);
    if (statement.kind === 'createTable') {
        if (schema) {
            if (statement.ifNotExists) return { rows: [], rowsAffected: 0 };
            throw new SqlSchemaError(`table already exists: ${statement.table}`);
        }
        const created: TableSchema = {
            version: 1,
            table: statement.table,
            columns: statement.columns,
            nextRowId: '1'
        };
        validateSchema(created, statement.table);
        if (!catalogExists) {
            await tx.createStore(CATALOG);
            await tx.put(CATALOG, OWNER_KEY, jsonEncode(OWNER));
        }
        await tx.createStore(statement.table);
        await tx.put(statement.table, TABLE_OWNER_KEY, tableOwnerValue(created));
        await tx.put(CATALOG, schemaKey(statement.table), jsonEncode(created));
        return { rows: [], rowsAffected: 0 };
    }
    if (!schema) {
        if (statement.kind === 'dropTable' && statement.ifExists) return { rows: [], rowsAffected: 0 };
        throw new SqlSchemaError(`table does not exist: ${statement.table}`);
    }
    if (statement.kind === 'dropTable') {
        await tx.dropStore(statement.table);
        await tx.delete(CATALOG, schemaKey(statement.table));
        return { rows: [], rowsAffected: 0 };
    }
    if (statement.kind === 'insert') {
        return insertRows(tx, schema, statement, parameters);
    }
    validateWhere(schema, statement.where, parameters);
    if (statement.kind === 'select') {
        validateColumnList(schema, statement.columns);
        for (const order of statement.orderBy) getColumn(schema, order.column);
    }
    if (statement.kind === 'update') {
        validateColumnList(
            schema,
            statement.assignments.map((assignment) => assignment.column)
        );
        for (const assignment of statement.assignments) {
            normalizeValue(getColumn(schema, assignment.column), resolveSqlValue(assignment.value, parameters));
        }
    }
    const orderBy = statement.kind === 'select' ? statement.orderBy : [];
    const plan = planAccess(schema, statement.where, orderBy, parameters, indexes);
    const limit = statement.kind === 'select' ? resolveCount(statement.limit, parameters, 'LIMIT') : null;
    const offset = statement.kind === 'select' ? (resolveCount(statement.offset, parameters, 'OFFSET') ?? 0) : 0;
    if (explain) return { rows: [], rowsAffected: 0, plan: plan.public };
    const maxMatches =
        statement.kind === 'select' && plan.public.sort === 'none' && limit !== null
            ? Math.min(Number.MAX_SAFE_INTEGER, offset + limit)
            : undefined;
    const rows = limit === 0 ? [] : await readRows(tx, schema, plan, statement.where, parameters, maxMatches);
    if (statement.kind === 'select') {
        if (plan.public.sort === 'memory') rows.sort((left, right) => compareRows(left, right, orderBy));
        const selected = rows.slice(offset, limit === null ? undefined : offset + limit);
        return {
            rows: selected.map(({ row }) =>
                projectRow(row, statement.columns ?? schema.columns.map((column) => column.name))
            ),
            rowsAffected: 0,
            plan: plan.public
        };
    }
    if (statement.kind === 'delete') {
        await tx.deleteMany(
            schema.table,
            rows.map(({ key }) => key)
        );
        return { rows: [], rowsAffected: rows.length, plan: plan.public };
    }
    const updates = rows.map(({ key, row }) => {
        const updated = { ...row };
        for (const assignment of statement.assignments) {
            updated[assignment.column] = normalizeValue(
                getColumn(schema, assignment.column),
                resolveSqlValue(assignment.value, parameters)
            );
        }
        const primary = primaryColumn(schema);
        return { oldKey: key, key: primary ? primaryKey(primary, updated[primary.name]) : key, row: updated };
    });
    const destinations = new Set<string>();
    const oldKeys = new Set(rows.map(({ key }) => bytesIdentity(key)));
    for (const update of updates) {
        const identity = bytesIdentity(update.key);
        if (destinations.has(identity)) throw new ConstraintError(`duplicate primary key in ${schema.table}`);
        destinations.add(identity);
        if (!oldKeys.has(identity) && (await tx.has(schema.table, update.key))) {
            throw new ConstraintError(`duplicate primary key in ${schema.table}`);
        }
    }
    await tx.deleteMany(
        schema.table,
        updates.map(({ oldKey }) => oldKey)
    );
    for (const update of updates) await tx.put(schema.table, update.key, encodeRow(schema, update.row));
    return { rows: [], rowsAffected: updates.length, plan: plan.public };
}

async function readCatalog(tx: Transaction): Promise<boolean> {
    let value: Uint8Array | null;
    try {
        value = await tx.get(CATALOG, OWNER_KEY);
    } catch (error) {
        if (error instanceof StoreNotFoundError || (error instanceof Error && error.name === 'StoreNotFoundError'))
            return false;
        throw error;
    }
    if (!value) throw new SqlSchemaError(`SQL catalog store is not owned by SQL: ${CATALOG}`);
    const owner: unknown = decodeJson(value);
    if (!isRecord(owner) || owner.format !== OWNER.format || owner.version !== OWNER.version) {
        throw new SqlSchemaError('invalid SQL catalog metadata');
    }
    return true;
}

async function readSchema(tx: Transaction, table: string): Promise<TableSchema | null> {
    const value = await tx.get(CATALOG, schemaKey(table));
    if (!value) return null;
    const schema: unknown = decodeJson(value);
    validateSchema(schema, table);
    return schema;
}

async function assertTableOwnership(tx: Transaction, schema: TableSchema): Promise<void> {
    let owner: Uint8Array | null;
    try {
        owner = await tx.get(schema.table, TABLE_OWNER_KEY);
    } catch (error) {
        if (error instanceof StoreNotFoundError || (error instanceof Error && error.name === 'StoreNotFoundError')) {
            throw new SqlSchemaError(`SQL table store is missing: ${schema.table}`, { cause: error });
        }
        throw error;
    }
    if (!owner || compareBytes(owner, tableOwnerValue(schema)) !== 0) {
        throw new SqlSchemaError(`SQL table ownership does not match its catalog: ${schema.table}`);
    }
}

function tableOwnerValue(schema: TableSchema): Uint8Array {
    return jsonEncode({
        [TABLE_OWNER_FIELD]: JSON.stringify({
            format: 'moyodb.sql.table',
            version: schema.version,
            table: schema.table,
            columns: schema.columns
        })
    });
}

function validateSchema(value: unknown, table: string): asserts value is TableSchema {
    if (
        !isRecord(value) ||
        value.version !== 1 ||
        value.table !== table ||
        !Array.isArray(value.columns) ||
        !value.columns.length ||
        typeof value.nextRowId !== 'string' ||
        value.nextRowId.length > 20 ||
        !/^[1-9]\d*$/u.test(value.nextRowId) ||
        BigInt(value.nextRowId) > ROW_ID_MAX + 1n
    ) {
        throw new SqlSchemaError(`invalid schema metadata for ${table}`);
    }
    const names = new Set<string>();
    let primaries = 0;
    for (const candidate of value.columns as unknown[]) {
        if (
            !isRecord(candidate) ||
            typeof candidate.name !== 'string' ||
            !candidate.name.length ||
            typeof candidate.type !== 'string' ||
            !COLUMN_TYPES.has(candidate.type) ||
            typeof candidate.primaryKey !== 'boolean' ||
            typeof candidate.nullable !== 'boolean' ||
            names.has(candidate.name) ||
            (candidate.primaryKey && candidate.nullable)
        ) {
            throw new SqlSchemaError(`invalid column metadata for ${table}`);
        }
        names.add(candidate.name);
        if (candidate.primaryKey) primaries += 1;
    }
    if (primaries > 1) throw new SqlSchemaError(`multiple primary keys in ${table}`);
}

async function insertRows(
    tx: Transaction,
    schema: TableSchema,
    statement: Extract<SqlStatement, { kind: 'insert' }>,
    parameters: SqlValue[]
): Promise<SqlResult> {
    const columns = statement.columns ?? schema.columns.map((column) => column.name);
    validateColumnList(schema, columns);
    const primary = primaryColumn(schema);
    let nextRowId = BigInt(schema.nextRowId);
    const entries: Array<[Uint8Array, Uint8Array]> = [];
    const keys = new Set<string>();
    for (const values of statement.rows) {
        if (values.length !== columns.length) throw new SqlSchemaError('INSERT column count does not match VALUES');
        const row = Object.create(null) as SqlRow;
        for (const column of schema.columns) row[column.name] = null;
        values.forEach((expression, index) => {
            row[columns[index]] = resolveSqlValue(expression, parameters);
        });
        for (const column of schema.columns) row[column.name] = normalizeValue(column, row[column.name]);
        if (!primary && nextRowId > ROW_ID_MAX)
            throw new ConstraintError(`row identifier space exhausted in ${schema.table}`);
        const key = primary ? primaryKey(primary, row[primary.name]) : u64Key(nextRowId++);
        const identity = bytesIdentity(key);
        if (keys.has(identity) || (await tx.has(schema.table, key)))
            throw new ConstraintError(`duplicate primary key in ${schema.table}`);
        keys.add(identity);
        entries.push([key, encodeRow(schema, row)]);
    }
    await tx.putMany(schema.table, entries);
    if (!primary) {
        schema.nextRowId = nextRowId.toString();
        await tx.put(CATALOG, schemaKey(schema.table), jsonEncode(schema));
    }
    return { rows: [], rowsAffected: entries.length };
}

function normalizeValue(column: SqlColumnDefinition, value: SqlValue): SqlValue {
    if (value === null) {
        if (!column.nullable) throw new ConstraintError(`column ${column.name} cannot be NULL`);
        return null;
    }
    switch (column.type) {
        case 'INTEGER': {
            if (typeof value !== 'bigint' && (typeof value !== 'number' || !Number.isSafeInteger(value))) break;
            const integer = typeof value === 'bigint' ? value : BigInt(value);
            if (integer < INTEGER_MIN || integer > INTEGER_MAX) break;
            return integer >= BigInt(Number.MIN_SAFE_INTEGER) && integer <= BigInt(Number.MAX_SAFE_INTEGER)
                ? Number(integer)
                : integer;
        }
        case 'REAL':
            if (typeof value === 'number' && Number.isFinite(value)) return Object.is(value, -0) ? 0 : value;
            if (typeof value === 'bigint') {
                const asNumber = Number(value);
                // Reject bigints Number would round. Exact powers of two above 2^53 stay finite REAL values.
                if (Number.isFinite(asNumber) && BigInt(asNumber) === value) return asNumber;
            }
            break;
        case 'TEXT':
            if (typeof value === 'string') {
                indexKey(value);
                return value;
            }
            break;
        case 'BOOLEAN':
            if (typeof value === 'boolean') return value;
            break;
        case 'BLOB':
            if (value instanceof Uint8Array) return value.slice();
            break;
    }
    throw new SqlTypeError(`column ${column.name} requires ${column.type}`);
}

function primaryKey(column: SqlColumnDefinition, value: SqlValue): Uint8Array {
    if (value === null) throw new ConstraintError(`primary key ${column.name} cannot be NULL`);
    if (column.type === 'INTEGER') return u64Key(BigInt(value as number | bigint) - INTEGER_MIN);
    if (typeof value === 'bigint') throw new SqlTypeError(`invalid primary key ${column.name}`);
    return indexKey(value);
}

function encodeRow(schema: TableSchema, row: SqlRow): Uint8Array {
    const stored = Object.create(null) as Record<string, unknown>;
    for (const column of schema.columns) stored[column.name] = storedValue(row[column.name]);
    return jsonEncode(stored);
}

function storedValue(value: SqlValue): null | boolean | number | string {
    // Keep native index fields scalar. The table schema restores integers and bytes.
    if (typeof value === 'bigint') return value.toString();
    if (value instanceof Uint8Array) return bytesIdentity(value);
    return value;
}

function decodeRow(schema: TableSchema, item: ScanItem): StoredRow {
    const stored: unknown = decodeJson(item.value);
    if (!isRecord(stored) || Object.keys(stored).length !== schema.columns.length)
        throw new SerializationError(`invalid SQL row in ${schema.table}`);
    const row = Object.create(null) as SqlRow;
    try {
        for (const column of schema.columns) {
            if (!Object.hasOwn(stored, column.name)) throw new Error('missing SQL column');
            let value = stored[column.name];
            if (column.type === 'INTEGER' && typeof value === 'string') {
                if (value.length > 20 || !/^-?(?:0|[1-9]\d*)$/u.test(value)) throw new Error('invalid stored integer');
                value = BigInt(value);
            } else if (column.type === 'BLOB' && typeof value === 'string') {
                if (value.length % 2 || !/^[0-9a-f]*$/u.test(value)) throw new Error('invalid stored blob');
                value = Uint8Array.from(value.match(/.{2}/gu) ?? [], (byte) => Number.parseInt(byte, 16));
            }
            if (
                value !== null &&
                typeof value !== 'boolean' &&
                typeof value !== 'number' &&
                typeof value !== 'string' &&
                typeof value !== 'bigint' &&
                !(value instanceof Uint8Array)
            )
                throw new Error('invalid stored value');
            row[column.name] = normalizeValue(column, value);
        }
        const primary = primaryColumn(schema);
        if (primary && compareBytes(primaryKey(primary, row[primary.name]), item.key) !== 0)
            throw new Error('SQL row primary key differs from storage key');
    } catch (error) {
        throw new SerializationError(
            `invalid SQL row in ${schema.table}: ${error instanceof Error ? error.message : 'invalid value'}`
        );
    }
    return { key: item.key, row };
}

function validateWhere(schema: TableSchema, expression: SqlExpression | null, parameters: SqlValue[]): void {
    if (!expression) return;
    if ('left' in expression) {
        validateWhere(schema, expression.left, parameters);
        validateWhere(schema, expression.right, parameters);
        return;
    }
    const column = getColumn(schema, expression.column);
    if (expression.kind === 'comparison') {
        const value = resolveSqlValue(expression.value, parameters);
        if (value === null) return;
        if (column.type === 'INTEGER' || column.type === 'REAL') {
            if (typeof value === 'bigint' || (typeof value === 'number' && Number.isFinite(value))) return;
            throw new SqlTypeError(`comparison for ${column.name} requires a numeric value`);
        }
        normalizeValue({ ...column, nullable: true }, value);
    }
}

function evaluateWhere(row: SqlRow, expression: SqlExpression, parameters: SqlValue[]): Truth {
    if ('left' in expression) {
        const left = evaluateWhere(row, expression.left, parameters);
        const right = evaluateWhere(row, expression.right, parameters);
        if (expression.kind === 'and')
            return left === false || right === false ? false : left === null || right === null ? null : true;
        return left === true || right === true ? true : left === null || right === null ? null : false;
    }
    const left = row[expression.column];
    if (expression.kind === 'isNull') return expression.negated ? left !== null : left === null;
    const right = resolveSqlValue(expression.value, parameters);
    if (left === null || right === null) return null;
    const compared = compareValues(left, right);
    switch (expression.operator) {
        case '=':
            return compared === 0;
        case '!=':
            return compared !== 0;
        case '<':
            return compared < 0;
        case '<=':
            return compared <= 0;
        case '>':
            return compared > 0;
        case '>=':
            return compared >= 0;
    }
}

function planAccess(
    schema: TableSchema,
    where: SqlExpression | null,
    orderBy: SqlOrderBy[],
    parameters: SqlValue[],
    indexes: NormalizedIndexDef[]
): AccessPlan {
    const comparisons = conjunctiveComparisons(where);
    const primary = primaryColumn(schema);
    const candidates: Array<{ column: SqlColumnDefinition; index?: NormalizedIndexDef }> = [];
    if (primary) candidates.push({ column: primary });
    for (const index of indexes) {
        // Native dotted paths address nested fields. SQL columns are flat properties.
        if (
            index.store === schema.table &&
            !index.compound &&
            index.keyPath.length === 1 &&
            !index.keyPath[0].includes('.')
        ) {
            const column = schema.columns.find((entry) => entry.name === index.keyPath[0]);
            if (column) candidates.push({ column, index });
        }
    }
    for (const equalityOnly of [true, false]) {
        for (const candidate of candidates) {
            const selected = comparisons.filter(
                (entry) =>
                    entry.column === candidate.column.name &&
                    (equalityOnly ? entry.operator === '=' : ['<', '<=', '>', '>='].includes(entry.operator))
            );
            let key: Uint8Array | undefined;
            const range: Range = {};
            for (const comparison of selected) {
                const value = resolveSqlValue(comparison.value, parameters);
                const encoded = planKey(candidate.column, value, Boolean(candidate.index), equalityOnly);
                if (!encoded) continue;
                if (equalityOnly) {
                    key = encoded;
                    break;
                }
                setRangeBound(range, comparison.operator, encoded);
            }
            if (!key && !Object.keys(range).length) continue;
            const access: SqlQueryPlan['access'] = candidate.index
                ? equalityOnly
                    ? 'index-lookup'
                    : 'index-range'
                : equalityOnly
                  ? 'primary-key-lookup'
                  : 'primary-key-range';
            const ordered = traversalOrder(schema, candidate.column, candidate.index, orderBy, key);
            if (ordered && orderBy[0]?.direction === 'desc') range.reverse = true;
            return {
                public: {
                    table: schema.table,
                    access,
                    column: candidate.column.name,
                    ...(candidate.index ? { index: candidate.index.name } : {}),
                    sort: ordered ? 'none' : 'memory',
                    reason: `use ${candidate.index ? `index ${candidate.index.name}` : 'primary key'} for ${equalityOnly ? 'equality' : 'range'}; apply remaining predicates after lookup`
                },
                key,
                range,
                empty: !key && emptyRange(range)
            };
        }
    }
    if (orderBy.length) {
        for (const candidate of candidates) {
            if (!traversalOrder(schema, candidate.column, candidate.index, orderBy)) continue;
            return {
                public: {
                    table: schema.table,
                    access: candidate.index ? 'index-range' : 'table-scan',
                    column: candidate.column.name,
                    ...(candidate.index ? { index: candidate.index.name } : {}),
                    sort: 'none',
                    reason: `use ${candidate.index ? `index ${candidate.index.name}` : 'primary key'} traversal order`
                },
                range: { reverse: orderBy[0].direction === 'desc' }
            };
        }
    }
    return {
        public: {
            table: schema.table,
            access: 'table-scan',
            sort: orderBy.length ? 'memory' : 'none',
            reason: 'no supported primary key or index bound; filter table rows'
        }
    };
}

function traversalOrder(
    schema: TableSchema,
    column: SqlColumnDefinition,
    index: NormalizedIndexDef | undefined,
    orders: SqlOrderBy[],
    key?: Uint8Array
): boolean {
    if (!orders.length || (key && !index)) return true;
    if (orders[0].column !== column.name) return false;
    if (!index) return orders.length === 1;
    // INTEGER indexes mix numeric keys and decimal strings for large values.
    if (column.type === 'INTEGER' && !key) return false;
    // SQL ties use primary ASC. Reverse index traversal reverses those ties.
    if (orders.length === 1) return orders[0].direction === 'asc' || (index.unique && !column.nullable);
    const primary = primaryColumn(schema);
    return orders.length === 2 && orders[1].column === primary?.name && orders[1].direction === orders[0].direction;
}

function planKey(
    column: SqlColumnDefinition,
    value: SqlValue,
    secondary: boolean,
    equality: boolean
): Uint8Array | null {
    if (value === null) return null;
    if (column.type === 'INTEGER') {
        if (typeof value !== 'bigint' && (typeof value !== 'number' || !Number.isSafeInteger(value))) return null;
        if (secondary && !equality) return null;
        const normalized = normalizeValue({ ...column, nullable: true }, value);
        return secondary ? indexKey(storedValue(normalized)) : primaryKey(column, normalized);
    }
    if (
        column.type === 'REAL' &&
        typeof value === 'bigint' &&
        (value < BigInt(Number.MIN_SAFE_INTEGER) || value > BigInt(Number.MAX_SAFE_INTEGER))
    )
        return null;
    const normalized = normalizeValue({ ...column, nullable: true }, value);
    return secondary ? indexKey(storedValue(normalized)) : primaryKey(column, normalized);
}

function conjunctiveComparisons(where: SqlExpression | null): Comparison[] {
    if (!where) return [];
    if (where.kind === 'comparison') return [where];
    if (where.kind === 'and') return [...conjunctiveComparisons(where.left), ...conjunctiveComparisons(where.right)];
    return [];
}

function setRangeBound(range: Range, operator: Comparison['operator'], key: Uint8Array): void {
    if (operator === '>' || operator === '>=') {
        const current = range.gt ?? range.gte;
        const compared = current ? compareBytes(key, current) : 1;
        if (compared > 0 || (compared === 0 && operator === '>')) {
            delete range.gt;
            delete range.gte;
            if (operator === '>') range.gt = key;
            else range.gte = key;
        }
    } else if (operator === '<' || operator === '<=') {
        const current = range.lt ?? range.lte;
        const compared = current ? compareBytes(key, current) : -1;
        if (compared < 0 || (compared === 0 && operator === '<')) {
            delete range.lt;
            delete range.lte;
            if (operator === '<') range.lt = key;
            else range.lte = key;
        }
    }
}

function emptyRange(range: Range): boolean {
    const lower = range.gt ?? range.gte;
    const upper = range.lt ?? range.lte;
    if (!lower || !upper) return false;
    const compared = compareBytes(lower, upper);
    return compared > 0 || (compared === 0 && (Boolean(range.gt) || Boolean(range.lt)));
}

async function readRows(
    tx: Transaction,
    schema: TableSchema,
    plan: AccessPlan,
    where: SqlExpression | null,
    parameters: SqlValue[],
    maxMatches?: number
): Promise<StoredRow[]> {
    if (plan.empty) return [];
    const rows: StoredRow[] = [];
    const accept = (item: ScanItem): boolean => {
        if (item.key.length === 1 && item.key[0] === TABLE_OWNER_KEY[0]) return false;
        const row = decodeRow(schema, item);
        if (!where || evaluateWhere(row.row, where, parameters) === true) rows.push(row);
        return maxMatches !== undefined && rows.length >= maxMatches;
    };
    if (plan.public.index) {
        const range = plan.key ? { ...plan.range, gte: plan.key, lte: plan.key } : plan.range;
        for await (const [key, value] of tx.scanByIndex(schema.table, plan.public.index, range, {
            maxRows: Math.min(256, maxMatches ?? 256)
        })) {
            if (accept({ key, value })) break;
        }
    } else if (plan.key) {
        const value = await tx.get(schema.table, plan.key);
        if (value) accept({ key: plan.key, value });
    } else if (typeof tx.scanPage === 'function' && typeof tx.closeScanCursor === 'function') {
        let cursor: number | undefined;
        try {
            for (;;) {
                const page = await tx.scanPage(schema.table, plan.range, {
                    cursor,
                    maxRows: Math.min(256, maxMatches === undefined ? 256 : maxMatches - rows.length)
                });
                cursor = page.cursor;
                for (const item of page.rows) {
                    if (accept(item)) return rows;
                }
                if (page.done) break;
            }
        } finally {
            if (cursor !== undefined) await tx.closeScanCursor(cursor);
        }
    } else {
        // Custom databases that implement the earlier Transaction API have no cursor methods.
        for (const item of await tx.scan(schema.table, plan.range)) {
            if (accept(item)) break;
        }
    }
    return rows;
}

function compareRows(left: StoredRow, right: StoredRow, orders: SqlOrderBy[]): number {
    for (const order of orders) {
        const compared = compareValues(left.row[order.column], right.row[order.column]);
        if (compared) return order.direction === 'desc' ? -compared : compared;
    }
    return compareBytes(left.key, right.key);
}

function compareValues(left: SqlValue, right: SqlValue): number {
    if (left === null) return right === null ? 0 : -1;
    if (right === null) return 1;
    if (
        (typeof left === 'number' || typeof left === 'bigint') &&
        (typeof right === 'number' || typeof right === 'bigint')
    )
        return left < right ? -1 : left > right ? 1 : 0;
    if (typeof left === 'string' && typeof right === 'string') return compareBytes(indexKey(left), indexKey(right));
    if (typeof left === 'boolean' && typeof right === 'boolean') return Number(left) - Number(right);
    if (left instanceof Uint8Array && right instanceof Uint8Array) return compareBytes(left, right);
    throw new SqlTypeError('comparison values have different SQL types');
}

function resolveCount(expression: SqlValueExpression | null, parameters: SqlValue[], clause: string): number | null {
    if (!expression) return null;
    const value = resolveSqlValue(expression, parameters);
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0)
        throw new SqlTypeError(`${clause} requires a non-negative safe integer`);
    return value;
}

function projectRow(row: SqlRow, columns: string[]): SqlRow {
    const result: SqlRow = {};
    for (const column of columns)
        Object.defineProperty(result, column, {
            value: row[column] instanceof Uint8Array ? row[column].slice() : row[column],
            enumerable: true,
            configurable: true,
            writable: true
        });
    return result;
}

function validateColumnList(schema: TableSchema, columns: string[] | null): void {
    if (!columns) return;
    const seen = new Set<string>();
    for (const name of columns) {
        getColumn(schema, name);
        if (seen.has(name)) throw new SqlSchemaError(`duplicate column: ${name}`);
        seen.add(name);
    }
}

function getColumn(schema: TableSchema, name: string): SqlColumnDefinition {
    const column = schema.columns.find((entry) => entry.name === name);
    if (!column) throw new SqlSchemaError(`unknown column: ${name}`);
    return column;
}

function primaryColumn(schema: TableSchema): SqlColumnDefinition | undefined {
    return schema.columns.find((column) => column.primaryKey);
}
function schemaKey(table: string): Uint8Array {
    return utf8Encode(`table:${table}`);
}
function assertTableName(table: string): void {
    if (table === CATALOG || table.startsWith('__browserdb:'))
        throw new SqlSchemaError(`reserved SQL table name: ${table}`);
}
function isRowStatement(statement: SqlStatement): boolean {
    return statement.kind === 'select' || statement.kind === 'update' || statement.kind === 'delete';
}
function isRecord(value: unknown): value is Record<string, unknown> {
    return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function decodeJson(value: Uint8Array): unknown {
    try {
        return jsonDecode<unknown>(value);
    } catch {
        throw new SerializationError('invalid SQL JSON data');
    }
}
function bytesIdentity(bytes: Uint8Array): string {
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}
function compareBytes(left: Uint8Array, right: Uint8Array): number {
    for (let index = 0; index < Math.min(left.length, right.length); index += 1)
        if (left[index] !== right[index]) return left[index] - right[index];
    return left.length - right.length;
}
