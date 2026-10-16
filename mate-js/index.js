// mate-js: use o Mate como banco de dados no Node.js.
//
//   const { Database } = require("./mate-js");
//   const db = new Database("app.db");
//   db.exec("CREATE TABLE users (id INT PRIMARY KEY, nome TEXT)");
//   db.run("INSERT INTO users VALUES (?, ?)", [1, "Ana"]);
//   db.get("SELECT * FROM users WHERE id = ?", [1]); // { id: 1, nome: "Ana" }
//
// O trabalho pesado acontece no módulo nativo (mate.node, compilado de
// src/lib.rs). Este arquivo só oferece uma API confortável por cima dele,
// no estilo do better-sqlite3: síncrona, porque o banco roda dentro do
// próprio processo e responde em microssegundos.

"use strict";

const path = require("node:path");

let native;
try {
  native = require(path.join(__dirname, "mate.node"));
} catch (err) {
  throw new Error(
    "mate.node não encontrado. Compile o módulo nativo primeiro: `npm run build` dentro de mate-js/",
    { cause: err },
  );
}

/** Erro vindo do banco. `code` diz o tipo: CONSTRAINT, PARSE, BUSY... */
class MateError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "MateError";
    this.code = code;
  }
}

/**
 * Chama o módulo nativo, trocando os erros dele por MateError. O lado
 * Rust põe o código na frente da mensagem: "CONSTRAINT: valor duplicado...".
 */
function call(fn) {
  try {
    return fn();
  } catch (err) {
    const match = /^([A-Z]+): ([\s\S]*)$/.exec(err.message);
    if (match) throw new MateError(match[1], match[2]);
    throw err;
  }
}

/** Transforma uma linha (array) num objeto { coluna: valor }. */
function toObject(columns, row) {
  const obj = {};
  for (let i = 0; i < columns.length; i++) obj[columns[i]] = row[i];
  return obj;
}

class Database {
  #native;

  /** Abre (ou cria) o banco no arquivo indicado. */
  constructor(filename) {
    if (typeof filename !== "string") {
      throw new TypeError("informe o caminho do arquivo do banco");
    }
    this.#native = call(() => new native.NativeDatabase(filename));
    this.filename = filename;
  }

  /** Executa um ou mais comandos separados por `;`. Sem parâmetros. */
  exec(sql) {
    call(() => this.#native.exec(sql));
    return this;
  }

  /** INSERT/UPDATE/DELETE (ou qualquer comando): devolve `{ changes }`. */
  run(sql, params = []) {
    const result = this.#query(sql, params);
    return { changes: result.changes ?? 0 };
  }

  /** Todas as linhas, como objetos `{ coluna: valor }`. */
  all(sql, params = []) {
    const { columns, rows } = this.#select(sql, params);
    return rows.map((row) => toObject(columns, row));
  }

  /** A primeira linha (ou `undefined` se não houver nenhuma). */
  get(sql, params = []) {
    const { columns, rows } = this.#select(sql, params);
    return rows.length > 0 ? toObject(columns, rows[0]) : undefined;
  }

  /**
   * As linhas como arrays, junto com os nomes das colunas. Útil quando duas
   * colunas têm o mesmo nome (num JOIN, por exemplo), e num objeto uma
   * sobrescreveria a outra.
   */
  values(sql, params = []) {
    return this.#select(sql, params);
  }

  /**
   * Embrulha uma função numa transação. Se ela terminar normalmente, COMMIT.
   * Se lançar um erro, ROLLBACK e o erro segue adiante.
   *
   *   const transferir = db.transaction((de, para, valor) => { ... });
   *   transferir(1, 2, 100);
   */
  transaction(fn) {
    const db = this;
    return function (...args) {
      db.exec("BEGIN");
      try {
        const result = fn.apply(this, args);
        db.exec("COMMIT");
        return result;
      } catch (err) {
        // Um erro do banco dentro da transação já a desfaz sozinho.
        if (db.inTransaction) db.exec("ROLLBACK");
        throw err;
      }
    };
  }

  /** Há uma transação aberta (BEGIN sem COMMIT/ROLLBACK)? */
  get inTransaction() {
    return this.#native.inTransaction;
  }

  /** Fecha o banco e libera o arquivo para outros processos. */
  close() {
    this.#native.close();
  }

  #query(sql, params) {
    if (!Array.isArray(params)) {
      throw new TypeError("os parâmetros devem ser um array: db.all(sql, [a, b])");
    }
    return call(() => this.#native.query(sql, params));
  }

  #select(sql, params) {
    const result = this.#query(sql, params);
    if (!result.rows) {
      throw new MateError("PLAN", "o comando não devolve linhas; use run() ou exec()");
    }
    return { columns: result.columns, rows: result.rows };
  }
}

module.exports = { Database, MateError };
