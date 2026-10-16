// Exemplo: uma lista de tarefas guardada no Mate.
// Rode com: node exemplo.js   (depois de `npm run build`)

"use strict";

const { Database, MateError } = require(".");

const db = new Database("tarefas.db");

db.exec(`
  CREATE TABLE IF NOT EXISTS tarefas (
    id        INT PRIMARY KEY,
    titulo    TEXT NOT NULL,
    feita     BOOLEAN NOT NULL,
    prioridade INT
  );
`);

// Próximo id livre.
const { proximo } = db.get("SELECT COALESCE(MAX(id), 0) + 1 AS proximo FROM tarefas");

// Várias inserções numa transação: entra tudo ou nada.
const adicionar = db.transaction((itens) => {
  itens.forEach(([titulo, prioridade], i) => {
    db.run("INSERT INTO tarefas VALUES (?, ?, ?, ?)", [proximo + i, titulo, false, prioridade]);
  });
});
adicionar([
  ["Ler storage/pager.rs", 1],
  ["Entender o split da B+Tree", 2],
  ["Fazer o exercício da free list", 3],
]);

db.run("UPDATE tarefas SET feita = ? WHERE id = ?", [true, proximo]);

console.log("Pendentes:");
for (const t of db.all("SELECT id, titulo FROM tarefas WHERE feita = ? ORDER BY prioridade", [false])) {
  console.log(`  #${t.id} ${t.titulo}`);
}

const { total } = db.get("SELECT COUNT(*) AS total FROM tarefas");
console.log(`Total de tarefas no banco: ${total}`);

// Erros do banco têm um código.
try {
  db.run("INSERT INTO tarefas VALUES (?, ?, ?, ?)", [proximo, "Repetida", false, 1]);
} catch (err) {
  if (err instanceof MateError) console.log(`Erro esperado (${err.code}): ${err.message}`);
}

db.close();
