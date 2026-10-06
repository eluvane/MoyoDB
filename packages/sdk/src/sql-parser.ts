import {
    SQL_INTEGER_MAX,
    SQL_INTEGER_MIN,
    type SqlColumnDefinition,
    type SqlColumnType,
    type SqlComparisonOperator,
    type SqlExpression,
    type SqlOrderBy,
    type SqlParseResult,
    type SqlStatement,
    type SqlValue,
    type SqlValueExpression
} from './sql-types';

const MAX_SQL_LENGTH = 1_048_576;
const MAX_TOKENS = 16_384;
const MAX_NESTING = 64;
const MAX_PREDICATES = 128;
const MAX_IDENTIFIER_LENGTH = 128;
const MAX_NUMBER_LENGTH = 1_024;
const FORBIDDEN_IDENTIFIERS = new Set(['__proto__', 'prototype', 'constructor']);
const COLUMN_TYPES = new Set<SqlColumnType>(['INTEGER', 'REAL', 'TEXT', 'BOOLEAN', 'BLOB']);
const KEYWORDS = new Set([
    'CREATE',
    'TABLE',
    'IF',
    'NOT',
    'EXISTS',
    'DROP',
    'INSERT',
    'INTO',
    'VALUES',
    'SELECT',
    'FROM',
    'WHERE',
    'ORDER',
    'BY',
    'ASC',
    'DESC',
    'LIMIT',
    'OFFSET',
    'UPDATE',
    'SET',
    'DELETE',
    'PRIMARY',
    'KEY',
    'NULL',
    'TRUE',
    'FALSE',
    'IS',
    'AND',
    'OR',
    'JOIN',
    'LEFT',
    'RIGHT',
    'INNER',
    'OUTER',
    'CROSS',
    'ON',
    'AS',
    'DISTINCT',
    'GROUP',
    'HAVING',
    'UNION',
    'ALL',
    'IN',
    'LIKE',
    'BETWEEN',
    'DEFAULT',
    'UNIQUE',
    'CHECK',
    'REFERENCES',
    'CONSTRAINT',
    'FOREIGN',
    'AUTOINCREMENT',
    'RETURNING',
    'CONFLICT',
    ...COLUMN_TYPES
]);

type TokenKind = 'word' | 'identifier' | 'string' | 'number' | 'symbol' | 'parameter' | 'end';
interface Token {
    kind: TokenKind;
    text: string;
    position: number;
}

export class SqlSyntaxError extends SyntaxError {
    readonly position: number;

    constructor(message: string, position: number) {
        super(`${message} at position ${position}`);
        this.name = 'SqlSyntaxError';
        this.position = position;
    }
}

export function parseSql(sql: string): SqlParseResult {
    if (typeof sql !== 'string') {
        throw new TypeError('SQL must be a string');
    }
    if (sql.length > MAX_SQL_LENGTH) {
        throw new SqlSyntaxError(`SQL exceeds ${MAX_SQL_LENGTH} characters`, MAX_SQL_LENGTH);
    }
    return new Parser(tokenize(sql)).parse();
}

export function bindSqlParameters(parsed: SqlParseResult, parameters: readonly unknown[] = []): SqlValue[] {
    if (!Array.isArray(parameters)) {
        throw new TypeError('SQL parameters must be an array');
    }
    if (parameters.length !== parsed.parameterCount) {
        throw new RangeError(`SQL expects ${parsed.parameterCount} parameters; received ${parameters.length}`);
    }
    return Array.from<unknown, SqlValue>(parameters, (value, index) => {
        if (value === null || typeof value === 'boolean') {
            return value;
        }
        if (typeof value === 'number') {
            if (
                !Number.isFinite(value) ||
                (Number.isInteger(value) && !Number.isSafeInteger(value) && fitsSigned64(value))
            ) {
                throw new RangeError(
                    `SQL parameter ${index + 1} must be finite; use bigint for integers outside the safe range`
                );
            }
            return value;
        }
        if (typeof value === 'bigint') {
            if (value < SQL_INTEGER_MIN || value > SQL_INTEGER_MAX) {
                throw new RangeError(`SQL parameter ${index + 1} exceeds the signed 64-bit integer range`);
            }
            return value;
        }
        if (typeof value === 'string') {
            if (!hasValidUnicode(value)) {
                throw new TypeError(`SQL parameter ${index + 1} must contain valid Unicode scalar values`);
            }
            return value;
        }
        if (value instanceof Uint8Array) {
            return Uint8Array.from(value);
        }
        throw new TypeError(`SQL parameter ${index + 1} must be null, boolean, number, bigint, string, or Uint8Array`);
    });
}

export function resolveSqlValue(expression: SqlValueExpression, parameters: readonly SqlValue[]): SqlValue {
    if (expression.kind === 'literal') {
        return expression.value;
    }
    if (!Number.isSafeInteger(expression.index) || expression.index < 0 || expression.index >= parameters.length) {
        throw new RangeError(`SQL parameter ${expression.index + 1} is missing`);
    }
    const value = parameters.at(expression.index);
    if (value === undefined) {
        throw new RangeError(`SQL parameter ${expression.index + 1} is missing`);
    }
    return value;
}

class Parser {
    #tokens: Token[];
    #index = 0;
    #parameterCount = 0;
    #nesting = 0;
    #predicates = 0;

    constructor(tokens: Token[]) {
        this.#tokens = tokens;
    }

    parse(): SqlParseResult {
        const statement = this.#parseStatement();
        this.#acceptSymbol(';');
        if (this.#current().kind !== 'end') {
            throw this.#error('Unexpected or unsupported SQL syntax');
        }
        return { statement, parameterCount: this.#parameterCount };
    }

    #parseStatement(): SqlStatement {
        if (this.#acceptKeyword('CREATE')) {
            return this.#createTable();
        }
        if (this.#acceptKeyword('DROP')) {
            return this.#dropTable();
        }
        if (this.#acceptKeyword('INSERT')) {
            return this.#insert();
        }
        if (this.#acceptKeyword('SELECT')) {
            return this.#select();
        }
        if (this.#acceptKeyword('UPDATE')) {
            return this.#update();
        }
        if (this.#acceptKeyword('DELETE')) {
            return this.#delete();
        }
        throw this.#error('Expected CREATE TABLE, DROP TABLE, INSERT, SELECT, UPDATE, or DELETE');
    }

    #createTable(): SqlStatement {
        this.#expectKeyword('TABLE');
        const ifNotExists = this.#acceptKeyword('IF');
        if (ifNotExists) {
            this.#expectKeyword('NOT');
            this.#expectKeyword('EXISTS');
        }
        const table = this.#identifier();
        this.#expectSymbol('(');
        const columns: SqlColumnDefinition[] = [];
        const names = new Set<string>();
        let primaryKeyCount = 0;
        do {
            const name = this.#identifier();
            this.#assertUnique(names, name, 'column');
            const typeToken = this.#current();
            const type = typeToken.text.toUpperCase() as SqlColumnType;
            if (typeToken.kind !== 'word' || !COLUMN_TYPES.has(type)) {
                throw this.#error('Expected INTEGER, REAL, TEXT, BOOLEAN, or BLOB column type');
            }
            this.#index += 1;
            let primaryKey = false;
            let nullable = true;
            let nullabilitySpecified = false;
            for (;;) {
                if (this.#acceptKeyword('PRIMARY')) {
                    if (primaryKey) {
                        throw this.#error('Duplicate PRIMARY KEY constraint');
                    }
                    this.#expectKeyword('KEY');
                    primaryKey = true;
                    primaryKeyCount += 1;
                    if (primaryKeyCount > 1) {
                        throw this.#error('Only one inline PRIMARY KEY is supported');
                    }
                } else if (this.#acceptKeyword('NOT')) {
                    this.#expectKeyword('NULL');
                    if (nullabilitySpecified) {
                        throw this.#error('Duplicate nullability constraint');
                    }
                    nullable = false;
                    nullabilitySpecified = true;
                } else if (this.#acceptKeyword('NULL')) {
                    if (nullabilitySpecified) {
                        throw this.#error('Duplicate nullability constraint');
                    }
                    nullable = true;
                    nullabilitySpecified = true;
                } else {
                    break;
                }
            }
            if (primaryKey && nullable && nullabilitySpecified) {
                throw this.#error('PRIMARY KEY cannot be declared NULL');
            }
            columns.push({ name, type, primaryKey, nullable: primaryKey ? false : nullable });
        } while (this.#acceptSymbol(','));
        this.#expectSymbol(')');
        return { kind: 'createTable', table, columns, ifNotExists };
    }

    #dropTable(): SqlStatement {
        this.#expectKeyword('TABLE');
        const ifExists = this.#acceptKeyword('IF');
        if (ifExists) {
            this.#expectKeyword('EXISTS');
        }
        return { kind: 'dropTable', table: this.#identifier(), ifExists };
    }

    #insert(): SqlStatement {
        this.#expectKeyword('INTO');
        const table = this.#identifier();
        let columns: string[] | null = null;
        if (this.#acceptSymbol('(')) {
            columns = this.#identifierList();
            this.#expectSymbol(')');
        }
        this.#expectKeyword('VALUES');
        const rows: SqlValueExpression[][] = [];
        let width = columns?.length;
        do {
            this.#expectSymbol('(');
            const row = [this.#value()];
            while (this.#acceptSymbol(',')) {
                row.push(this.#value());
            }
            this.#expectSymbol(')');
            width ??= row.length;
            if (row.length !== width) {
                throw this.#error('INSERT rows must match the column count');
            }
            rows.push(row);
        } while (this.#acceptSymbol(','));
        return { kind: 'insert', table, columns, rows };
    }

    #select(): SqlStatement {
        const columns = this.#acceptSymbol('*') ? null : this.#identifierList();
        this.#expectKeyword('FROM');
        const table = this.#identifier();
        const where = this.#where();
        const orderBy: SqlOrderBy[] = [];
        if (this.#acceptKeyword('ORDER')) {
            this.#expectKeyword('BY');
            const names = new Set<string>();
            do {
                const column = this.#identifier();
                this.#assertUnique(names, column, 'ORDER BY column');
                const direction = this.#acceptKeyword('DESC') ? 'desc' : 'asc';
                if (direction === 'asc') {
                    this.#acceptKeyword('ASC');
                }
                orderBy.push({ column, direction });
            } while (this.#acceptSymbol(','));
        }
        let limit: SqlValueExpression | null = null;
        let offset: SqlValueExpression | null = null;
        if (this.#acceptKeyword('LIMIT')) {
            limit = this.#rowCount('LIMIT');
            if (this.#acceptKeyword('OFFSET')) {
                offset = this.#rowCount('OFFSET');
            }
        }
        return { kind: 'select', table, columns, where, orderBy, limit, offset };
    }

    #update(): SqlStatement {
        const table = this.#identifier();
        this.#expectKeyword('SET');
        const assignments: { column: string; value: SqlValueExpression }[] = [];
        const names = new Set<string>();
        do {
            const column = this.#identifier();
            this.#assertUnique(names, column, 'assignment');
            this.#expectSymbol('=');
            assignments.push({ column, value: this.#value() });
        } while (this.#acceptSymbol(','));
        return { kind: 'update', table, assignments, where: this.#where() };
    }

    #delete(): SqlStatement {
        this.#expectKeyword('FROM');
        return { kind: 'delete', table: this.#identifier(), where: this.#where() };
    }

    #where(): SqlExpression | null {
        return this.#acceptKeyword('WHERE') ? this.#or() : null;
    }

    #or(): SqlExpression {
        let expression = this.#and();
        while (this.#acceptKeyword('OR')) {
            expression = { kind: 'or', left: expression, right: this.#and() };
        }
        return expression;
    }

    #and(): SqlExpression {
        let expression = this.#predicate();
        while (this.#acceptKeyword('AND')) {
            expression = { kind: 'and', left: expression, right: this.#predicate() };
        }
        return expression;
    }

    #predicate(): SqlExpression {
        if (this.#acceptSymbol('(')) {
            this.#nesting += 1;
            if (this.#nesting > MAX_NESTING) {
                throw this.#error(`WHERE nesting exceeds ${MAX_NESTING}`);
            }
            const expression = this.#or();
            this.#expectSymbol(')');
            this.#nesting -= 1;
            return expression;
        }
        this.#predicates += 1;
        if (this.#predicates > MAX_PREDICATES) {
            throw this.#error(`WHERE predicates exceed ${MAX_PREDICATES}`);
        }
        const column = this.#identifier();
        if (this.#acceptKeyword('IS')) {
            const negated = this.#acceptKeyword('NOT');
            this.#expectKeyword('NULL');
            return { kind: 'isNull', column, negated };
        }
        const token = this.#current();
        if (token.kind !== 'symbol' || !['=', '!=', '<>', '<', '<=', '>', '>='].includes(token.text)) {
            throw this.#error('Expected a comparison operator or IS [NOT] NULL');
        }
        this.#index += 1;
        const operator = (token.text === '<>' ? '!=' : token.text) as SqlComparisonOperator;
        return { kind: 'comparison', column, operator, value: this.#value() };
    }

    #rowCount(clause: string): SqlValueExpression {
        const value = this.#value();
        if (
            value.kind === 'literal' &&
            (typeof value.value !== 'number' || !Number.isSafeInteger(value.value) || value.value < 0)
        ) {
            throw this.#error(`${clause} must be a non-negative safe integer or parameter`);
        }
        return value;
    }

    #value(): SqlValueExpression {
        const token = this.#current();
        if (token.kind === 'parameter') {
            this.#index += 1;
            return { kind: 'parameter', index: this.#parameterCount++ };
        }
        if (token.kind === 'string') {
            this.#index += 1;
            return { kind: 'literal', value: token.text };
        }
        if (this.#acceptKeyword('NULL')) {
            return { kind: 'literal', value: null };
        }
        if (this.#acceptKeyword('TRUE')) {
            return { kind: 'literal', value: true };
        }
        if (this.#acceptKeyword('FALSE')) {
            return { kind: 'literal', value: false };
        }
        const sign = this.#acceptSymbol('-') ? '-' : this.#acceptSymbol('+') ? '+' : '';
        const number = this.#current();
        if (number.kind === 'number') {
            this.#index += 1;
            return { kind: 'literal', value: parseNumber(sign + number.text, token.position) };
        }
        throw this.#error('Expected a scalar literal or positional ? parameter');
    }

    #identifierList(): string[] {
        const names = new Set<string>();
        const columns: string[] = [];
        do {
            const name = this.#identifier();
            this.#assertUnique(names, name, 'column');
            columns.push(name);
        } while (this.#acceptSymbol(','));
        return columns;
    }

    #identifier(): string {
        const token = this.#current();
        if (token.kind !== 'identifier' && (token.kind !== 'word' || KEYWORDS.has(token.text.toUpperCase()))) {
            throw this.#error('Expected an identifier; use double quotes for keywords');
        }
        if (
            token.text.length === 0 ||
            token.text.length > MAX_IDENTIFIER_LENGTH ||
            hasControlCharacters(token.text) ||
            FORBIDDEN_IDENTIFIERS.has(token.text.toLowerCase()) ||
            !hasValidUnicode(token.text)
        ) {
            throw this.#error('Invalid or reserved identifier');
        }
        this.#index += 1;
        return token.text;
    }

    #assertUnique(names: Set<string>, name: string, label: string): void {
        if (names.has(name)) {
            throw this.#error(`Duplicate ${label}: ${name}`);
        }
        names.add(name);
    }

    #current(): Token {
        return this.#tokens[this.#index];
    }

    #acceptKeyword(keyword: string): boolean {
        const token = this.#current();
        if (token.kind !== 'word' || token.text.toUpperCase() !== keyword) {
            return false;
        }
        this.#index += 1;
        return true;
    }

    #expectKeyword(keyword: string): void {
        if (!this.#acceptKeyword(keyword)) {
            throw this.#error(`Expected ${keyword}`);
        }
    }

    #acceptSymbol(symbol: string): boolean {
        const token = this.#current();
        if (token.kind !== 'symbol' || token.text !== symbol) {
            return false;
        }
        this.#index += 1;
        return true;
    }

    #expectSymbol(symbol: string): void {
        if (!this.#acceptSymbol(symbol)) {
            throw this.#error(`Expected ${symbol}`);
        }
    }

    #error(message: string): SqlSyntaxError {
        return new SqlSyntaxError(message, this.#current().position);
    }
}

function tokenize(sql: string): Token[] {
    const tokens: Token[] = [];
    let index = 0;
    while (index < sql.length) {
        const character = sql[index];
        if (/\s/u.test(character)) {
            index += 1;
            continue;
        }
        if (sql.startsWith('--', index)) {
            index += 2;
            while (index < sql.length && sql[index] !== '\n' && sql[index] !== '\r') {
                index += 1;
            }
            continue;
        }
        if (sql.startsWith('/*', index)) {
            const end = sql.indexOf('*/', index + 2);
            if (end < 0) {
                throw new SqlSyntaxError('Unterminated block comment', index);
            }
            index = end + 2;
            continue;
        }
        if (tokens.length >= MAX_TOKENS) {
            throw new SqlSyntaxError(`SQL token count exceeds ${MAX_TOKENS}`, index);
        }
        const position = index;
        if (character === "'" || character === '"') {
            const quote = character;
            let text = '';
            let closed = false;
            index += 1;
            while (index < sql.length) {
                if (sql[index] === quote) {
                    if (sql[index + 1] === quote) {
                        text += quote;
                        index += 2;
                    } else {
                        index += 1;
                        closed = true;
                        break;
                    }
                } else {
                    text += sql[index++];
                }
            }
            if (!closed) {
                throw new SqlSyntaxError('Unterminated quoted token', position);
            }
            if (!hasValidUnicode(text)) {
                throw new SqlSyntaxError('Quoted token must contain valid Unicode scalar values', position);
            }
            tokens.push({ kind: quote === "'" ? 'string' : 'identifier', text, position });
        } else if (/[A-Za-z_]/u.test(character)) {
            index += 1;
            while (index < sql.length && /\w/u.test(sql[index])) {
                index += 1;
            }
            tokens.push({ kind: 'word', text: sql.slice(position, index), position });
        } else if (/\d/u.test(character) || (character === '.' && /\d/u.test(sql[index + 1] ?? ''))) {
            index += 1;
            while (index < sql.length && /\d/u.test(sql[index])) {
                index += 1;
            }
            if (character !== '.' && sql[index] === '.') {
                index += 1;
                while (index < sql.length && /\d/u.test(sql[index])) {
                    index += 1;
                }
            }
            if (sql[index] === 'e' || sql[index] === 'E') {
                index += 1;
                if (sql[index] === '+' || sql[index] === '-') {
                    index += 1;
                }
                const exponentStart = index;
                while (index < sql.length && /\d/u.test(sql[index])) {
                    index += 1;
                }
                if (index === exponentStart) {
                    throw new SqlSyntaxError('Invalid numeric exponent', position);
                }
            }
            const text = sql.slice(position, index);
            if (text.length > MAX_NUMBER_LENGTH) {
                throw new SqlSyntaxError(`Numeric literal exceeds ${MAX_NUMBER_LENGTH} characters`, position);
            }
            tokens.push({ kind: 'number', text, position });
        } else if (character === '?') {
            index += 1;
            tokens.push({ kind: 'parameter', text: character, position });
        } else {
            const pair = sql.slice(index, index + 2);
            if (['<=', '>=', '<>', '!='].includes(pair)) {
                index += 2;
                tokens.push({ kind: 'symbol', text: pair, position });
            } else if ('(),;*=<>+-'.includes(character)) {
                index += 1;
                tokens.push({ kind: 'symbol', text: character, position });
            } else {
                throw new SqlSyntaxError(`Unsupported SQL character: ${character}`, position);
            }
        }
    }
    tokens.push({ kind: 'end', text: '', position: sql.length });
    return tokens;
}

function fitsSigned64(value: number): boolean {
    if (!Number.isInteger(value)) {
        return false;
    }
    const integer = BigInt(value);
    return integer >= SQL_INTEGER_MIN && integer <= SQL_INTEGER_MAX;
}

function parseNumber(text: string, position: number): number | bigint {
    if (/^[+-]?\d+$/u.test(text)) {
        const integer = BigInt(text);
        if (integer < SQL_INTEGER_MIN || integer > SQL_INTEGER_MAX) {
            throw new SqlSyntaxError('Integer literal exceeds the signed 64-bit range', position);
        }
        return integer >= BigInt(Number.MIN_SAFE_INTEGER) && integer <= BigInt(Number.MAX_SAFE_INTEGER)
            ? Number(integer)
            : integer;
    }
    const value = Number(text);
    // Magnitudes at or above 2^63 are finite REAL values and do not fit in INTEGER.
    if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value) && fitsSigned64(value))) {
        throw new SqlSyntaxError('REAL literal must be finite; use integer notation for large integers', position);
    }
    const [mantissa, exponentText = '0'] = text.toLowerCase().split('e');
    const digits = mantissa.replace(/[+.-]/gu, '');
    const nonzero = /[1-9]/u.test(digits);
    if (value === 0 && nonzero) {
        throw new SqlSyntaxError('REAL literal underflows to zero', position);
    }
    if (Number.isInteger(value) && nonzero) {
        const fractionLength = mantissa.includes('.') ? mantissa.length - mantissa.indexOf('.') - 1 : 0;
        const trailingZeros = digits.length - digits.replace(/0+$/u, '').length;
        if (Number(exponentText) - fractionLength + trailingZeros < 0) {
            throw new SqlSyntaxError('REAL literal loses its fractional part', position);
        }
    }
    return value;
}

function hasValidUnicode(value: string): boolean {
    for (const character of value) {
        const codePoint = character.codePointAt(0) as number;
        if (codePoint >= 0xd800 && codePoint <= 0xdfff) {
            return false;
        }
    }
    return true;
}

function hasControlCharacters(value: string): boolean {
    for (const character of value) {
        const codePoint = character.codePointAt(0) as number;
        if (codePoint < 0x20 || codePoint === 0x7f) {
            return true;
        }
    }
    return false;
}
