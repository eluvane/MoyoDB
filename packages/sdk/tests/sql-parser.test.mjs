import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import ts from 'typescript';

const compile = async (name) => {
    const source = await readFile(new URL(`../src/${name}.ts`, import.meta.url), 'utf8');
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
const typesUrl = `data:text/javascript;base64,${Buffer.from(await compile('sql-types')).toString('base64')}`;
const parserSource = (await compile('sql-parser')).replace("from './sql-types'", `from '${typesUrl}'`);
const { parseSql, bindSqlParameters, resolveSqlValue, SqlSyntaxError } = await import(
    `data:text/javascript;base64,${Buffer.from(parserSource).toString('base64')}`
);
const literal = (value) => ({ kind: 'literal', value });
const comparison = (column, operator, value) => ({ kind: 'comparison', column, operator, value: literal(value) });
const rejects = (sql) => assert.throws(() => parseSql(sql), SqlSyntaxError);

test('CREATE TABLE parses typed columns, constraints and conditional creation', () => {
    assert.deepEqual(
        parseSql(
            'cReAtE TABLE IF NOT EXISTS People (id INTEGER PRIMARY KEY, name TEXT NOT NULL, ok BOOLEAN, data BLOB NULL, amount REAL);'
        ),
        {
            parameterCount: 0,
            statement: {
                kind: 'createTable',
                table: 'People',
                ifNotExists: true,
                columns: [
                    { name: 'id', type: 'INTEGER', primaryKey: true, nullable: false },
                    { name: 'name', type: 'TEXT', primaryKey: false, nullable: false },
                    { name: 'ok', type: 'BOOLEAN', primaryKey: false, nullable: true },
                    { name: 'data', type: 'BLOB', primaryKey: false, nullable: true },
                    { name: 'amount', type: 'REAL', primaryKey: false, nullable: true }
                ]
            }
        }
    );
    assert.deepEqual(parseSql('DROP TABLE IF EXISTS People').statement, {
        kind: 'dropTable',
        table: 'People',
        ifExists: true
    });
    assert.equal(parseSql('DROP TABLE People').statement.ifExists, false);
});

test('quoted identifiers, escaped strings and comments remain tokens', () => {
    const parsed = parseSql(`/* header */ INSERT INTO "odd table" ("select", "quote""column")
        VALUES ('it''s ?; -- /* safe */', ?), ('/*not comment*/', NULL); -- tail`);
    assert.deepEqual(parsed, {
        parameterCount: 1,
        statement: {
            kind: 'insert',
            table: 'odd table',
            columns: ['select', 'quote"column'],
            rows: [
                [literal("it's ?; -- /* safe */"), { kind: 'parameter', index: 0 }],
                [literal('/*not comment*/'), literal(null)]
            ]
        }
    });
    assert.equal(parseSql('SELECT "emoji🙂" FROM "таблица"').statement.table, 'таблица');
});

test('AND binds tighter than OR and parentheses change grouping', () => {
    assert.deepEqual(parseSql('SELECT * FROM docs WHERE a = 1 OR b <> 2 AND c >= 3').statement.where, {
        kind: 'or',
        left: comparison('a', '=', 1),
        right: { kind: 'and', left: comparison('b', '!=', 2), right: comparison('c', '>=', 3) }
    });
    assert.deepEqual(parseSql('SELECT * FROM docs WHERE (a = 1 OR b = 2) AND c = 3').statement.where, {
        kind: 'and',
        left: { kind: 'or', left: comparison('a', '=', 1), right: comparison('b', '=', 2) },
        right: comparison('c', '=', 3)
    });
});

test('all comparison operators and null predicates have explicit AST forms', () => {
    for (const operator of ['=', '!=', '<', '<=', '>', '>=']) {
        assert.deepEqual(
            parseSql(`SELECT * FROM docs WHERE a ${operator} -2`).statement.where,
            comparison('a', operator, -2)
        );
    }
    assert.deepEqual(parseSql('DELETE FROM docs WHERE value IS NULL OR other IS NOT NULL').statement.where, {
        kind: 'or',
        left: { kind: 'isNull', column: 'value', negated: false },
        right: { kind: 'isNull', column: 'other', negated: true }
    });
    rejects('SELECT * FROM docs WHERE value IS TRUE');
    rejects('SELECT * FROM docs WHERE value IS NOT ?');
});

test('SELECT parses projection, multiple sort terms, limit and offset', () => {
    assert.deepEqual(
        parseSql('SELECT name, id FROM docs WHERE ok = TRUE ORDER BY name DESC, id ASC LIMIT ? OFFSET 3'),
        {
            parameterCount: 1,
            statement: {
                kind: 'select',
                table: 'docs',
                columns: ['name', 'id'],
                where: comparison('ok', '=', true),
                orderBy: [
                    { column: 'name', direction: 'desc' },
                    { column: 'id', direction: 'asc' }
                ],
                limit: { kind: 'parameter', index: 0 },
                offset: literal(3)
            }
        }
    );
    assert.deepEqual(parseSql('SELECT * FROM docs').statement, {
        kind: 'select',
        table: 'docs',
        columns: null,
        where: null,
        orderBy: [],
        limit: null,
        offset: null
    });
});

test('UPDATE and DELETE retain assignments and optional WHERE', () => {
    assert.deepEqual(parseSql('UPDATE docs SET name = ?, ok = FALSE WHERE id = ?'), {
        parameterCount: 2,
        statement: {
            kind: 'update',
            table: 'docs',
            assignments: [
                { column: 'name', value: { kind: 'parameter', index: 0 } },
                { column: 'ok', value: literal(false) }
            ],
            where: { kind: 'comparison', column: 'id', operator: '=', value: { kind: 'parameter', index: 1 } }
        }
    });
    assert.equal(parseSql('DELETE FROM docs').statement.where, null);
    assert.equal(parseSql('UPDATE docs SET name = NULL').statement.where, null);
});

test('INSERT accepts omitted columns and enforces a consistent nonempty row width', () => {
    assert.deepEqual(parseSql('INSERT INTO docs VALUES (1, ?), (2, ?), (3, ?)').statement.rows, [
        [literal(1), { kind: 'parameter', index: 0 }],
        [literal(2), { kind: 'parameter', index: 1 }],
        [literal(3), { kind: 'parameter', index: 2 }]
    ]);
    for (const sql of [
        'INSERT INTO docs VALUES ()',
        'INSERT INTO docs (a, b) VALUES (1)',
        'INSERT INTO docs VALUES (1), (2, 3)',
        'INSERT INTO docs () VALUES (1)',
        'INSERT INTO docs VALUES (1),'
    ]) {
        rejects(sql);
    }
});

test('parameters bind as scalars, with exact arity and copied blobs', () => {
    const parsed = parseSql('INSERT INTO docs VALUES (?, ?, ?, ?, ?, ?, ?)');
    const blob = new Uint8Array([1, 2, 3]);
    const parameters = bindSqlParameters(parsed, [
        null,
        true,
        2.5,
        9223372036854775807n,
        "'; DROP TABLE docs; --",
        blob,
        '🙂'
    ]);
    assert.deepEqual(parameters, [null, true, 2.5, 9223372036854775807n, "'; DROP TABLE docs; --", blob, '🙂']);
    blob[0] = 99;
    assert.equal(parameters[5][0], 1);
    assert.equal(resolveSqlValue({ kind: 'parameter', index: 4 }, parameters), "'; DROP TABLE docs; --");
    assert.equal(resolveSqlValue(literal(12), []), 12);
    assert.throws(() => bindSqlParameters(parsed, []), RangeError);
    assert.throws(() => bindSqlParameters(parseSql('SELECT * FROM docs'), [1]), RangeError);
    assert.throws(() => bindSqlParameters(parsed, null), TypeError);
    assert.throws(() => resolveSqlValue({ kind: 'parameter', index: -1 }, [1]), RangeError);
    assert.throws(() => resolveSqlValue({ kind: 'parameter', index: 1 }, [1]), RangeError);
});

test('parameter holes, undefined, objects and lossy numbers are rejected', () => {
    const parsed = parseSql('INSERT INTO docs VALUES (?)');
    for (const value of [
        undefined,
        {},
        [],
        new Date(),
        new ArrayBuffer(1),
        NaN,
        Infinity,
        -Infinity,
        9007199254740992,
        9223372036854775808n,
        -9223372036854775809n,
        '\ud800'
    ]) {
        assert.throws(() => bindSqlParameters(parsed, [value]));
    }
    assert.throws(() => bindSqlParameters(parsed, new Array(1)), TypeError);
    const buffer = Buffer.from([4, 5]);
    const bound = bindSqlParameters(parsed, [buffer]);
    buffer[0] = 100;
    assert.equal(bound[0][0], 4);
});

test('signed 64-bit integer literals retain precision across safe number boundaries', () => {
    const rows = parseSql(
        'INSERT INTO docs VALUES (9007199254740991), (9007199254740992), (-9007199254740992), (9223372036854775807), (-9223372036854775808)'
    ).statement.rows;
    assert.deepEqual(
        rows.map((row) => row[0].value),
        [9007199254740991, 9007199254740992n, -9007199254740992n, 9223372036854775807n, -9223372036854775808n]
    );
    rejects('INSERT INTO docs VALUES (9223372036854775808)');
    rejects('INSERT INTO docs VALUES (-9223372036854775809)');
});

test('REAL syntax accepts bounded numbers and rejects overflow or hidden fractional loss', () => {
    assert.deepEqual(
        parseSql('INSERT INTO docs VALUES (.5, 1., +2.25, -3e-2, 4e2, 5e-324, 0e999)').statement.rows[0].map(
            (value) => value.value
        ),
        [0.5, 1, 2.25, -0.03, 400, 5e-324, 0]
    );
    for (const value of [
        '1e309',
        '-1e309',
        '1e-999',
        '9007199254740992.0',
        '9007199254740991.1',
        '1.00000000000000000001',
        '1e',
        '1e+',
        '1e-',
        '0x10',
        'NaN',
        'Infinity'
    ]) {
        rejects(`INSERT INTO docs VALUES (${value})`);
    }
});

test('LIMIT and OFFSET require non-negative safe integers', () => {
    for (const sql of [
        'SELECT * FROM docs LIMIT -1',
        'SELECT * FROM docs LIMIT .5',
        "SELECT * FROM docs LIMIT '1'",
        'SELECT * FROM docs LIMIT NULL',
        'SELECT * FROM docs LIMIT 9007199254740992',
        'SELECT * FROM docs LIMIT 1 OFFSET -1',
        'SELECT * FROM docs OFFSET 1',
        'SELECT * FROM docs LIMIT 1, 2'
    ]) {
        rejects(sql);
    }
    assert.equal(parseSql('SELECT * FROM docs LIMIT 0 OFFSET 0').statement.limit.value, 0);
});

test('unsupported SQL cannot silently become a supported statement', () => {
    for (const sql of [
        'SELECT * FROM docs; DELETE FROM docs',
        'SELECT * FROM docs;;',
        'SELECT DISTINCT name FROM docs',
        'SELECT COUNT(*) FROM docs',
        'SELECT id AS alias FROM docs',
        'SELECT d.id FROM docs d',
        'SELECT * FROM docs JOIN other ON docs.id = other.id',
        'SELECT * FROM docs GROUP BY id',
        'SELECT * FROM docs WHERE id IN (1, 2)',
        "SELECT * FROM docs WHERE name LIKE 'x%'",
        'SELECT * FROM docs WHERE NOT id = 1',
        'SELECT * FROM docs WHERE id = other',
        'SELECT * FROM docs WHERE id = (SELECT id FROM other)',
        'UPDATE docs SET id = id + 1',
        'UPDATE docs SET id = 1 RETURNING id',
        'DELETE FROM docs ORDER BY id LIMIT 1',
        'INSERT OR REPLACE INTO docs VALUES (1)',
        'CREATE INDEX by_id ON docs (id)',
        'CREATE TABLE docs (id INTEGER DEFAULT 1)',
        'CREATE TABLE docs (id INTEGER UNIQUE)',
        'CREATE TABLE docs (id VARCHAR(10))',
        'CREATE TABLE docs (id INTEGER, PRIMARY KEY (id))',
        'INSERT INTO docs VALUES (?1)',
        'INSERT INTO docs VALUES ($1)',
        'INSERT INTO docs VALUES (:name)',
        'INSERT INTO docs VALUES (@name)',
        'INSERT INTO docs VALUES (+?)',
        "INSERT INTO docs VALUES (X'ff')"
    ]) {
        rejects(sql);
    }
});

test('duplicate names and conflicting constraints are rejected', () => {
    for (const sql of [
        'CREATE TABLE docs ()',
        'CREATE TABLE docs (id INTEGER, id TEXT)',
        'CREATE TABLE docs (id INTEGER PRIMARY KEY, other INTEGER PRIMARY KEY)',
        'CREATE TABLE docs (id INTEGER NULL NOT NULL)',
        'CREATE TABLE docs (id INTEGER NOT NULL NOT NULL)',
        'CREATE TABLE docs (id INTEGER PRIMARY KEY PRIMARY KEY)',
        'CREATE TABLE docs (id INTEGER PRIMARY KEY NULL)',
        'SELECT id, id FROM docs',
        'SELECT * FROM docs ORDER BY id, id DESC',
        'UPDATE docs SET id = 1, id = 2',
        'INSERT INTO docs (id, id) VALUES (1, 2)'
    ]) {
        rejects(sql);
    }
});

test('dangerous property names and invalid identifiers are rejected in each position', () => {
    for (const name of [
        '__proto__',
        'constructor',
        'prototype',
        'Constructor',
        '"__proto__"',
        '"constructor"',
        '""',
        '"bad\u0000name"',
        '"bad\nname"',
        `"${'x'.repeat(129)}"`
    ]) {
        rejects(`SELECT * FROM ${name}`);
        rejects(`SELECT ${name} FROM docs`);
        rejects(`UPDATE docs SET ${name} = 1`);
        rejects(`CREATE TABLE docs (${name} TEXT)`);
    }
    rejects('SELECT * FROM `docs`');
    rejects('SELECT * FROM [docs]');
    rejects('SELECT * FROM "\ud800"');
    rejects("INSERT INTO docs VALUES ('\ud800')");
});

test('invalid syntax and unterminated lexical tokens report a source position', () => {
    for (const sql of [
        '',
        ' ',
        '-- comment',
        'SELECT',
        'SELECT * FROM',
        'SELECT * FROM docs WHERE',
        "INSERT INTO docs VALUES ('open)",
        'SELECT * FROM "open',
        'SELECT * FROM docs /* open',
        'SELECT * FROM docs WHERE id == 1',
        'SELECT * FROM docs WHERE id = 1 garbage'
    ]) {
        assert.throws(
            () => parseSql(sql),
            (error) =>
                error instanceof SqlSyntaxError &&
                Number.isSafeInteger(error.position) &&
                error.position >= 0 &&
                error.position <= sql.length
        );
    }
    assert.throws(() => parseSql(null), TypeError);
});

test('input length, token count, numeric length and WHERE depth are bounded', () => {
    rejects(`SELECT * FROM docs /*${'x'.repeat(1_048_576)}*/`);
    rejects(`SELECT * FROM docs WHERE ${'('.repeat(65)}id = 1${')'.repeat(65)}`);
    rejects(`SELECT * FROM docs WHERE ${Array.from({ length: 129 }, () => 'id = 1').join(' OR ')}`);
    rejects(`INSERT INTO docs VALUES (${'1'.repeat(1_025)})`);
    rejects(`INSERT INTO docs VALUES ${Array.from({ length: 4_100 }, () => '(1)').join(',')}`);
    assert.equal(
        parseSql(`SELECT * FROM docs WHERE ${'('.repeat(64)}id = 1${')'.repeat(64)}`).statement.where.column,
        'id'
    );
});
