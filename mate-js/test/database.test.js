// Testes da biblioteca JS. Rode com `npm test` dentro de mate-js/
// (depois de `npm run build`).

"use strict";

const { test, beforeEach, afterEach } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const { Database, MateError } = require("..");

let dir;
let db;

beforeEach(() => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), "mate-js-"));
  db = new Database(path.join(dir, "test.db"));
  db.exec(`
    CREATE TABLE users (id INT PRIMARY KEY, nome TEXT NOT NULL, idade INT, saldo FLOAT, ativo BOOLEAN);
    INSERT INTO users VALUES
      (1, 'Ana', 30, 10.5, TRUE),
      (2, 'Bruno', 17, 0, FALSE),
      (3, 'Carla', NULL, NULL, TRUE);
  `);
});

afterEach(() => {
  db.close();
  fs.rmSync(dir, { recursive: true, force: true });
});

test("get e all devolvem objetos com os tipos do JS", () => {
  assert.deepEqual(db.get("SELECT * FROM users WHERE id = ?", [1]), {
    id: 1,
    nome: "Ana",
    idade: 30,
    saldo: 10.5,
    ativo: true,
  });
  assert.equal(db.get("SELECT * FROM users WHERE id = ?", [99]), undefined);
  assert.deepEqual(
    db.all("SELECT nome FROM users WHERE ativo = ? ORDER BY nome", [true]),
    [{ nome: "Ana" }, { nome: "Carla" }],
  );
  assert.deepEqual(db.get("SELECT idade, saldo FROM users WHERE id = 3"), { idade: null, saldo: null });
});

test("run devolve quantas linhas mudaram", () => {
  assert.deepEqual(db.run("INSERT INTO users VALUES (?, ?, ?, ?, ?)", [4, "Davi", 22, 1.25, false]), {
    changes: 1,
  });
  assert.deepEqual(db.run("UPDATE users SET idade = idade + ? WHERE idade IS NOT NULL", [1]), { changes: 3 });
  assert.deepEqual(db.run("DELETE FROM users WHERE id > ?", [2]), { changes: 2 });
  assert.equal(db.get("SELECT COUNT(*) AS n FROM users").n, 2);
});

test("parâmetros protegem contra SQL injection", () => {
  const malicioso = "x' OR '1'='1";
  assert.equal(db.all("SELECT * FROM users WHERE nome = ?", [malicioso]).length, 0);
  db.run("INSERT INTO users (id, nome) VALUES (?, ?)", [10, malicioso]);
  assert.equal(db.get("SELECT id FROM users WHERE nome = ?", [malicioso]).id, 10);
});

test("números: inteiros, floats e BigInt", () => {
  db.exec("CREATE TABLE n (i INT, f FLOAT)");
  db.run("INSERT INTO n VALUES (?, ?)", [9007199254740993n, 2]); // além de 2^53
  const row = db.get("SELECT i, f FROM n");
  assert.equal(row.i, 9007199254740993n);
  assert.equal(row.f, 2);
  // 2.5 não cabe numa coluna INTEGER.
  assert.throws(() => db.run("INSERT INTO n VALUES (?, 1)", [2.5]), { code: "EXECUTION" });
});

test("erros viram MateError com código", () => {
  assert.throws(() => db.run("INSERT INTO users (id, nome) VALUES (1, 'dup')"), (err) => {
    assert.ok(err instanceof MateError);
    assert.equal(err.code, "CONSTRAINT");
    assert.match(err.message, /duplicado/);
    return true;
  });
  assert.throws(() => db.all("SELEC * FROM users"), { code: "PARSE" });
  assert.throws(() => db.all("SELECT * FROM nada"), { code: "PLAN" });
  assert.throws(() => db.all("SELECT * FROM users WHERE id = ?", []), { code: "PLAN" });
  assert.throws(() => db.all("SELECT 1 / 0"), { code: "EXECUTION" });
  assert.throws(() => db.run("SELECT 1", [{}]), /tipo não suportado/);
});

test("transaction confirma ou desfaz tudo", () => {
  const transferir = db.transaction((de, para, valor) => {
    db.run("UPDATE users SET saldo = saldo - ? WHERE id = ?", [valor, de]);
    db.run("UPDATE users SET saldo = saldo + ? WHERE id = ?", [valor, para]);
    if (db.get("SELECT saldo FROM users WHERE id = ?", [de]).saldo < 0) {
      throw new Error("saldo insuficiente");
    }
  });

  transferir(1, 2, 4);
  assert.equal(db.get("SELECT saldo FROM users WHERE id = 2").saldo, 4);

  assert.throws(() => transferir(1, 2, 100), /saldo insuficiente/);
  assert.equal(db.inTransaction, false);
  assert.equal(db.get("SELECT saldo FROM users WHERE id = 1").saldo, 6.5); // nada mudou
});

test("os dados sobrevivem a fechar e reabrir", () => {
  const file = db.filename;
  db.run("INSERT INTO users (id, nome) VALUES (?, ?)", [50, "Persistente"]);
  db.close();
  db = new Database(file);
  assert.equal(db.get("SELECT nome FROM users WHERE id = 50").nome, "Persistente");
});

test("o arquivo fica travado enquanto o banco está aberto", () => {
  assert.throws(() => new Database(db.filename), { code: "BUSY" });
});

test("values serve para colunas com o mesmo nome", () => {
  db.exec("CREATE TABLE pedidos (id INT PRIMARY KEY, user_id INT)");
  db.run("INSERT INTO pedidos VALUES (100, 1)");
  const { columns, rows } = db.values(
    "SELECT u.id, p.id FROM users u JOIN pedidos p ON p.user_id = u.id",
  );
  assert.deepEqual(columns, ["id", "id"]);
  assert.deepEqual(rows, [[1, 100]]);
});

test("banco fechado não aceita mais comandos", () => {
  db.close();
  assert.throws(() => db.all("SELECT 1"), { code: "CLOSED" });
  db = new Database(path.join(dir, "outro.db")); // para o afterEach
});
