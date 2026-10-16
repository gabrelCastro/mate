# Mate

[![CI](https://github.com/gabrelCastro/mate/actions/workflows/ci.yml/badge.svg)](https://github.com/gabrelCastro/mate/actions/workflows/ci.yml)

Um SGBD relacional **didático**, escrito em Rust para ser **lido**.

> O nome é uma homenagem ao Mate, o guincho enferrujado de *Carros*.
> Um banco feito em **Rust** não podia se chamar outra coisa.

São cerca de 7 mil linhas, sem nenhuma dependência externa, com todas as
peças de um banco de verdade: páginas em disco, cache, WAL com recuperação
após queda, heap com slotted pages, índices B+Tree, catálogo, parser SQL,
planner com escolha de índice, executores no modelo Volcano e transações.

Cada arquivo começa com um comentário explicando **o conceito** que
implementa (com diagramas), e o código prefere clareza a desempenho.
Comentários e mensagens estão em português.

## Rodando

```bash
cargo run -- meu.db                      # REPL interativo
cargo run -- meu.db < examples/demo.sql  # executa um script
cargo test                               # 57 testes (inclui o diferencial contra o SQLite)
cargo doc --open                         # a documentação navegável
```

```text
mate> CREATE TABLE users (id INT PRIMARY KEY, nome TEXT NOT NULL, idade INT);
CREATE TABLE users

mate> INSERT INTO users VALUES (1, 'Ana', 30), (2, 'Bruno', 17), (3, 'Carla', 25);
INSERT: 3 linhas

mate> EXPLAIN SELECT nome FROM users WHERE id = 3;
 plano
-------------------------------------------------
 Projection: nome
   -> Filter: id = 3
     -> IndexScan: users usando users_pkey (= 3)
(3 linhas)
```

### Comandos do REPL para estudar o banco por dentro

| Comando             | O que mostra                                               |
|---------------------|------------------------------------------------------------|
| `EXPLAIN <sql>;`    | a árvore de operadores que vai executar a consulta         |
| `.btree <índice>`   | a B+Tree desenhada nível por nível, com o número das páginas |
| `.schema`           | tabelas, índices e em qual página cada um começa           |
| `.stats`            | acertos/faltas no cache, leituras/escritas, quadros no WAL |
| `SELECT * FROM mate_schema;` | o catálogo, que é uma tabela comum              |

### Usando no Node.js

A pasta [`mate-js/`](mate-js/README.md) transforma o Mate numa biblioteca
para Node.js, no estilo do `better-sqlite3`:

```js
const { Database } = require("./mate-js");
const db = new Database("app.db");
db.run("INSERT INTO users VALUES (?, ?)", [1, "Ana"]);
db.get("SELECT * FROM users WHERE id = ?", [1]); // { id: 1, nome: "Ana" }
```

## SQL suportado

```sql
CREATE TABLE [IF NOT EXISTS] t (col TIPO [PRIMARY KEY] [NOT NULL] [UNIQUE], ...)
CREATE [UNIQUE] INDEX nome ON t (col)
DROP TABLE [IF EXISTS] t
INSERT INTO t [(cols)] VALUES (...), (...)
SELECT [DISTINCT] ... FROM t [alias] [[INNER | LEFT] JOIN t2 ON ...]
       [WHERE ...] [GROUP BY ...] [HAVING ...] [ORDER BY ... [ASC|DESC]] [LIMIT n] [OFFSET n]
UPDATE t SET col = expr, ... [WHERE ...]
DELETE FROM t [WHERE ...]
BEGIN | COMMIT | ROLLBACK
EXPLAIN <comando>
```

Valores podem vir como **parâmetros** `?`, passados à parte (veja
[`sql/params.rs`](src/sql/params.rs)), o que protege contra SQL injection.

- **Tipos:** `INTEGER`, `FLOAT`, `TEXT`, `BOOLEAN` (e sinônimos como `INT`,
  `VARCHAR(n)`, `REAL`, `BOOL`).
- **Expressões:** `+ - * / %`, `||`, comparações, `AND OR NOT`,
  `IS [NOT] NULL`, `[NOT] IN (...)`, `[NOT] BETWEEN`, `[NOT] LIKE`.
- **Funções:** `UPPER`, `LOWER`, `LENGTH`, `ABS`, `COALESCE`.
- **Agregações:** `COUNT(*)`, `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`.

## Roteiro de leitura

A arquitetura é em camadas, e cada camada só conhece as que estão abaixo
dela. A melhor ordem é **de baixo para cima**: cada arquivo se apoia só no
que você já leu. O guia [`docs/arquitetura.md`](docs/arquitetura.md) mostra
o caminho completo de uma consulta pelas camadas.

| # | Arquivo | Conceito |
|---|---------|----------|
| 1 | [`storage/page.rs`](src/storage/page.rs) | a página de 4 KB, unidade de E/S |
| 2 | [`storage/disk.rs`](src/storage/disk.rs) | página N ↔ offset N×4096 no arquivo; `fsync` |
| 3 | [`storage/lru.rs`](src/storage/lru.rs) | cache de páginas com política LRU |
| 4 | [`storage/checksum.rs`](src/storage/checksum.rs) | CRC-32 para detectar gravações pela metade |
| 5 | [`storage/wal.rs`](src/storage/wal.rs) | **write-ahead log**: durabilidade e recuperação |
| 6 | [`storage/pager.rs`](src/storage/pager.rs) | cache + transações (no-steal) + WAL juntos |
| 7 | [`types/value.rs`](src/types/value.rs) | valores SQL, NULL, ordem total × ordem SQL |
| 8 | [`types/encoding.rs`](src/types/encoding.rs) | linhas viram bytes |
| 9 | [`heap/slotted_page.rs`](src/heap/slotted_page.rs) | várias linhas numa página; compactação |
| 10 | [`heap/table_heap.rs`](src/heap/table_heap.rs) | tabela = lista de páginas; `RecordId` |
| 11 | [`btree/node.rs`](src/btree/node.rs) | nós folha e interno da B+Tree em disco |
| 12 | [`btree/tree.rs`](src/btree/tree.rs) | **B+Tree**: busca, inserção com split, cursor |
| 13 | [`catalog/catalog.rs`](src/catalog/catalog.rs) | o catálogo é uma tabela (`mate_schema`) |
| 14 | [`sql/lexer.rs`](src/sql/lexer.rs) | texto → tokens |
| 15 | [`sql/ast.rs`](src/sql/ast.rs) | o SQL como estrutura de dados |
| 16 | [`sql/parser.rs`](src/sql/parser.rs) | parser descendente recursivo; precedência |
| 17 | [`sql/params.rs`](src/sql/params.rs) | parâmetros `?` e por que evitam SQL injection |
| 18 | [`execution/expr.rs`](src/execution/expr.rs) | binding e lógica de três valores |
| 19 | [`planner/plan.rs`](src/planner/plan.rs) | o plano como árvore de operadores |
| 20 | [`planner/planner.rs`](src/planner/planner.rs) | escopo, binding, agregação, escolha de índice |
| 21 | [`execution/mod.rs`](src/execution/mod.rs) | **modelo Volcano** (`next()`) |
| 22 | [`execution/scan.rs`](src/execution/scan.rs) | SeqScan × IndexScan |
| 23 | [`execution/transform.rs`](src/execution/transform.rs) | Filter, Projection, Distinct, Limit |
| 24 | [`execution/join.rs`](src/execution/join.rs) | nested loop join (INNER e LEFT) |
| 25 | [`execution/sort.rs`](src/execution/sort.rs) | operador bloqueante |
| 26 | [`execution/aggregate.rs`](src/execution/aggregate.rs) | GROUP BY com acumuladores |
| 27 | [`execution/modify.rs`](src/execution/modify.rs) | manter índices em dia; problema do Halloween |
| 28 | [`database.rs`](src/database.rs) | a fachada: autocommit, BEGIN/COMMIT/ROLLBACK |
| 29 | [`main.rs`](src/main.rs) | o REPL |

Se preferir começar pelo "alto nível", leia `database.rs` primeiro para ver
o fluxo inteiro e depois desça.

### Dicas de estudo

- **Rode os testes de cada módulo** enquanto lê. Eles estão no fim de cada
  arquivo e funcionam como exemplos executáveis:
  `cargo test btree::` ou `cargo test pager::`.
- **Quebre coisas de propósito.** Comente o `sync_all` do WAL, troque `<` por
  `<=` no `seek` da B+Tree, tire o "collect" do UPDATE... e veja quais testes
  falham, e por quê.
- **Observe com `.stats`**: rode a mesma consulta com e sem índice e compare
  quantas páginas foram lidas.
- **Faça os exercícios** em [`docs/exercicios.md`](docs/exercicios.md).

## Como sabemos que funciona

Além dos testes de cada módulo e dos testes de SQL em `tests/sql.rs`, há um
**teste diferencial** ([`tests/diferencial.rs`](tests/diferencial.rs)): ele
gera milhares de consultas e modificações aleatórias, roda cada uma no Mate e
no SQLite, e exige que os dois deem a mesma resposta. Se discordarem, o teste
mostra a consulta e a semente para reproduzir. O CI roda tudo a cada push.

O SQLite entra só como dependência **de teste**. O banco em si continua sem
nenhuma dependência.

## Simplificações deliberadas

Um banco de produção faz tudo isto de forma mais sofisticada. Aqui, cada
item foi simplificado para o código caber na cabeça. Os comentários no
código explicam como os bancos reais resolvem.

| Aqui | Em bancos reais |
|------|-----------------|
| Um processo por vez (trava no arquivo), sem concorrência | locks finos, MVCC, níveis de isolamento |
| Transação precisa caber na memória (no-steal) | steal + undo log (ARIES) |
| WAL com imagens de página inteiras | registros lógicos/físicos menores |
| Páginas liberadas nunca são reusadas | free list |
| B+Tree não funde nós após remoções | merge/redistribuição |
| Nós da B+Tree desserializados por inteiro | manipulação direta dos bytes |
| Índices só de uma coluna | índices compostos, parciais, de expressão |
| Otimizador com uma regra fixa | otimizador por custo com estatísticas |
| Só nested loop join | hash join, merge join |
| Sort e Aggregate inteiramente em memória | algoritmos externos (em disco) |
| Linha precisa caber numa página | páginas de overflow (TOAST no Postgres) |
