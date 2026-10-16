# mate-js

Use o [Mate](../README.md) como banco de dados no **Node.js**, do mesmo jeito
que se usa o SQLite com o `better-sqlite3`.

```js
const { Database } = require("./mate-js");

const db = new Database("app.db");
db.exec("CREATE TABLE IF NOT EXISTS users (id INT PRIMARY KEY, nome TEXT NOT NULL, idade INT)");

db.run("INSERT INTO users VALUES (?, ?, ?)", [1, "Ana", 30]);

db.get("SELECT * FROM users WHERE id = ?", [1]);          // { id: 1, nome: "Ana", idade: 30 }
db.all("SELECT nome FROM users WHERE idade > ?", [18]);    // [{ nome: "Ana" }]

db.close();
```

Funciona com `require` e com `import`, e vem com tipos TypeScript
(`index.d.ts`). Não tem nenhuma dependência npm.

## Compilando

O módulo nativo (`mate.node`) é compilado a partir do Rust. Você precisa do
[Rust](https://rustup.rs) e do Node 18 ou mais novo:

```bash
cd mate-js
npm run build     # compila e gera mate.node
npm test          # 10 testes, com o test runner embutido do Node
node exemplo.js   # uma lista de tarefas guardada no Mate
```

Para usar em outro projeto seu, aponte para esta pasta:
`require("/caminho/para/mate/mate-js")`, ou `npm install /caminho/para/mate/mate-js`.

## API

| Método | Devolve | Para quê |
|--------|---------|----------|
| `new Database(arquivo)` | | abre ou cria o banco (trava o arquivo para este processo) |
| `db.exec(sql)` | `db` | um ou mais comandos separados por `;`, sem parâmetros (CREATE, scripts) |
| `db.run(sql, params?)` | `{ changes }` | INSERT, UPDATE, DELETE |
| `db.get(sql, params?)` | objeto ou `undefined` | a primeira linha |
| `db.all(sql, params?)` | array de objetos | todas as linhas |
| `db.values(sql, params?)` | `{ columns, rows }` | linhas como arrays (quando colunas têm o mesmo nome) |
| `db.transaction(fn)` | função | embrulha `fn` em BEGIN/COMMIT, com ROLLBACK se lançar erro |
| `db.inTransaction` | boolean | há uma transação aberta? |
| `db.close()` | | fecha e libera o arquivo |

### Parâmetros `?`

**Sempre** passe valores vindos de fora como parâmetros, nunca colados no
texto do SQL:

```js
db.get("SELECT * FROM users WHERE nome = ?", [nomeDoFormulario]);   // seguro
db.get(`SELECT * FROM users WHERE nome = '${nomeDoFormulario}'`);    // SQL injection!
```

### Tipos

| Mate | JavaScript |
|------|------------|
| `INTEGER` | `number` (ou `bigint`, se passar de 2⁵³) |
| `FLOAT` | `number` |
| `TEXT` | `string` |
| `BOOLEAN` | `boolean` |
| `NULL` | `null` |

Um `number` inteiro vira `INTEGER` (e é aceito em colunas `FLOAT`), e um
`number` com casas decimais vira `FLOAT`.

### Erros

Erros do banco são `MateError`, com um `code`:

```js
const { MateError } = require("./mate-js");
try {
  db.run("INSERT INTO users VALUES (?, ?, ?)", [1, "Repetido", 20]);
} catch (err) {
  if (err instanceof MateError && err.code === "CONSTRAINT") { /* id duplicado */ }
}
```

Códigos: `PARSE` (SQL inválido), `PLAN` (tabela/coluna inexistente, número
errado de parâmetros), `EXECUTION` (tipo errado, divisão por zero),
`CONSTRAINT` (NOT NULL, UNIQUE, PRIMARY KEY), `TRANSACTION`, `BUSY` (arquivo
aberto em outro processo), `CLOSED`, `IO` e `CORRUPT`.

## Como funciona

```text
  seu código JS
       │  db.all("SELECT ... ?", [18])
       ▼
  index.js         API amigável: objetos, transaction(), MateError
       │  native.query(sql, params)
       ▼
  mate.node        src/lib.rs: converte JS ↔ Value via N-API (crate napi)
       │  db.query_with_params(sql, &values)
       ▼
  Mate (Rust)      parser → planner → executores → páginas no disco
```

- **N-API** é a interface em C que o Node oferece para módulos nativos. O
  crate `napi` deixa usá-la a partir do Rust: `#[napi]` numa struct a
  transforma numa classe JS.
- A API é **síncrona**: o banco roda dentro do seu processo e responde em
  microssegundos, então Promises só acrescentariam custo. O
  `better-sqlite3` faz a mesma escolha.

## Limites

São os mesmos do Mate (veja "Simplificações deliberadas" no README
principal): um processo por vez no mesmo arquivo, transações precisam caber
na memória e espaço apagado não é devolvido ao disco. Serve bem para
estudo, protótipos e apps pequenos. Para produção, prefira o SQLite.
