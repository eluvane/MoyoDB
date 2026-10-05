export const SQL_INTEGER_MIN = -(1n << 63n);
export const SQL_INTEGER_MAX = (1n << 63n) - 1n;

export type SqlValue = null | boolean | number | bigint | string | Uint8Array;
export type SqlColumnType = 'INTEGER' | 'REAL' | 'TEXT' | 'BOOLEAN' | 'BLOB';

export interface SqlColumnDefinition {
    name: string;
    type: SqlColumnType;
    primaryKey: boolean;
    nullable: boolean;
}

export type SqlValueExpression = { kind: 'literal'; value: SqlValue } | { kind: 'parameter'; index: number };
export type SqlComparisonOperator = '=' | '!=' | '<' | '<=' | '>' | '>=';
export type SqlExpression =
    | { kind: 'comparison'; column: string; operator: SqlComparisonOperator; value: SqlValueExpression }
    | { kind: 'isNull'; column: string; negated: boolean }
    | { kind: 'and' | 'or'; left: SqlExpression; right: SqlExpression };

export interface SqlOrderBy {
    column: string;
    direction: 'asc' | 'desc';
}

export interface SqlCreateTableStatement {
    kind: 'createTable';
    table: string;
    columns: SqlColumnDefinition[];
    ifNotExists: boolean;
}

export interface SqlDropTableStatement {
    kind: 'dropTable';
    table: string;
    ifExists: boolean;
}

export interface SqlInsertStatement {
    kind: 'insert';
    table: string;
    columns: string[] | null;
    rows: SqlValueExpression[][];
}

export interface SqlSelectStatement {
    kind: 'select';
    table: string;
    columns: string[] | null;
    where: SqlExpression | null;
    orderBy: SqlOrderBy[];
    limit: SqlValueExpression | null;
    offset: SqlValueExpression | null;
}

export interface SqlUpdateStatement {
    kind: 'update';
    table: string;
    assignments: { column: string; value: SqlValueExpression }[];
    where: SqlExpression | null;
}

export interface SqlDeleteStatement {
    kind: 'delete';
    table: string;
    where: SqlExpression | null;
}

export type SqlStatement =
    | SqlCreateTableStatement
    | SqlDropTableStatement
    | SqlInsertStatement
    | SqlSelectStatement
    | SqlUpdateStatement
    | SqlDeleteStatement;

export interface SqlParseResult {
    statement: SqlStatement;
    parameterCount: number;
}
