# Arquitetura do mate

Este guia acompanha **um comando** do momento em que você aperta Enter até o
dado chegar ao disco, passando por todas as camadas. Depois descreve o
formato dos arquivos e as decisões de projeto.

## As camadas

```text
  ┌──────────────────────────────────────────────────────────────┐
  │ main.rs           REPL: lê linhas até o ';'                   │
  ├──────────────────────────────────────────────────────────────┤
  │ database.rs       roteia o comando; autocommit / BEGIN        │
  ├────────────────┬────────────────────┬────────────────────────┤
  │ sql/           │ planner/           │ execution/             │
  │ texto → AST    │ AST → Plan         │ Plan → linhas          │
  ├────────────────┴────────────────────┴────────────────────────┤
  │ catalog/          quais tabelas e índices existem             │
  ├─────────────────────────────┬────────────────────────────────┤
  │ heap/  (onde as linhas moram) │ btree/  (índices)              │
  ├─────────────────────────────┴────────────────────────────────┤
  │ types/            Value ↔ bytes                               │
  ├──────────────────────────────────────────────────────────────┤
  │ storage/          Page, DiskManager, LruCache, Wal, Pager     │
  └──────────────────────────────────────────────────────────────┘
               arquivo.db            arquivo.db-wal
```

A regra de ouro: **cada camada só chama as de baixo**. O heap não sabe o
que é SQL, e a B+Tree não sabe que existe uma tabela. Só o pager toca no
disco.

## A viagem de um UPDATE

Vamos seguir este comando, numa tabela `users(id INT PRIMARY KEY, nome TEXT,
idade INT)` que já tem algumas linhas:

```sql
UPDATE users SET idade = idade + 1 WHERE id = 1;
```

### 1. Lexer: texto → tokens (`sql/lexer.rs`)

```text
[UPDATE] [Ident users] [SET] [Ident idade] [=] [Ident idade] [+] [Integer 1]
[WHERE] [Ident id] [=] [Integer 1] [;]
```

### 2. Parser: tokens → AST (`sql/parser.rs`)

```text
Statement::Update {
    table: "users",
    assignments: [("idade", Binary(Column idade, Add, Literal 1))],
    where_clause: Binary(Column id, Eq, Literal 1),
}
```

Nada foi validado ainda: o parser nem sabe se `users` existe.

### 3. Database: decide o caminho (`database.rs`)

É um UPDATE, então vai para o planner e depois para os executores. Não há
`BEGIN` aberto, então o comando é sua própria transação (**autocommit**).

### 4. Planner: AST → Plan (`planner/planner.rs`)

1. Busca `users` no catálogo e monta o **escopo**: `id` é a coluna 0,
   `nome` a 1, `idade` a 2.
2. Faz o **binding**: `idade + 1` vira `#2 + 1`, e `id = 1` vira `#0 = 1`.
3. **Otimização**: o WHERE tem `coluna = constante`, e `id` tem índice
   (`users_pkey`, criado pelo PRIMARY KEY). Então troca o SeqScan por um
   IndexScan.

```text
Update: users SET #2 = idade + 1
  -> Filter: id = 1
    -> IndexScan: users usando users_pkey (= 1)
```

### 5. Execução: Volcano (`execution/`)

O executor do Update pede linhas ao Filter, que pede ao IndexScan:

1. **IndexScan** chama `btree.seek(>= 1)`, que desce da raiz até a folha
   certa lendo uma página por nível. A primeira entrada é
   `(1, RecordId(página 2, slot 0))`. Com o `RecordId`, ele lê a linha no
   heap e a decodifica: `[1, 'Ana', 30]`.
2. **Filter** avalia `#0 = 1` → `TRUE` e repassa a linha.
3. **Update** primeiro **coleta** todas as linhas (para evitar o problema do
   Halloween) e só depois altera cada uma (`execution/modify.rs`):
   - calcula a nova linha: `[1, 'Ana', 31]`;
   - `delete_row`: marca o slot antigo como removido no heap e tira
     `(1, rid antigo)` de cada índice;
   - `insert_row`: confere tipos, NOT NULL e UNIQUE, grava a linha no heap
     (ganha um `RecordId` novo) e insere `(1, rid novo)` em cada índice.

Cada alteração de página, seja no heap ou na B+Tree, é um `pager.write(id,
página)`. Nada foi para o disco ainda: as páginas alteradas ficam no
conjunto `dirty` do pager.

### 6. Commit (`storage/pager.rs` e `storage/wal.rs`)

Sem erros, o `Database` chama `pager.commit()`:

1. Todas as páginas sujas são gravadas no **WAL** (`arquivo.db-wal`) como
   quadros, e o último leva a marca de COMMIT.
2. `fsync` no WAL. **A partir daqui a transação é durável.**
3. As páginas são copiadas para o `arquivo.db`, sem fsync.
4. A cada 1000 quadros, um **checkpoint**: fsync no `.db` e WAL esvaziado.

Se o processo cair entre os passos 2 e 4, a próxima abertura do banco lê o
WAL e reaplica as transações confirmadas (**recuperação**).

Se algo tivesse dado errado no passo 5 (um UNIQUE violado, por exemplo),
`pager.rollback()` simplesmente descartaria o conjunto `dirty`. Como nada
foi para o disco, não há nada a desfazer.

## Formato dos arquivos

### `arquivo.db`: um vetor de páginas de 4096 bytes

```text
 página 0      cabeçalho: "MATEDB01" + número de páginas
 página 1      heap da tabela de sistema mate_schema (o catálogo)
 página 2...   heaps das tabelas e nós das B+Trees, na ordem de alocação
```

Os tipos de página, todos documentados no topo do arquivo correspondente:

| Página | Layout definido em |
|--------|--------------------|
| cabeçalho (página 0) | `storage/pager.rs` |
| heap (slotted page) | `heap/slotted_page.rs` |
| nó da B+Tree (folha ou interno) | `btree/node.rs` |

E dentro delas, linhas e chaves são serializadas como descrito em
`types/encoding.rs`.

### `arquivo.db-wal`: quadros de 12 + 4096 bytes

```text
 [id da página][é commit?][CRC-32][4096 bytes da página] ...
```

Detalhes em `storage/wal.rs`.

### Experimente

```bash
cargo run -- /tmp/x.db <<< "CREATE TABLE t (a INT PRIMARY KEY); INSERT INTO t VALUES (42);"
xxd /tmp/x.db | less
```

Procure a assinatura `MATEDB01` no offset 0, o texto do `CREATE TABLE` na
página 1 (offset `0x1000`) e o valor 42 (`2a`) na página 2 (offset `0x2000`).

## Decisões de projeto (e o que bancos reais fazem)

**Por que copiar páginas em vez de usar referências?** O pager devolve
`&Page`, e quem quer alterar faz `clone()`, modifica e chama `write()`.
Bancos reais devolvem um "guard" que mantém a página fixada (*pinned*) no
buffer pool e travada (*latch*) contra outras threads, para alterar no
lugar. Isso é mais rápido, mas exige gerenciar tempo de vida, concorrência e
expulsão do cache ao mesmo tempo. Com a cópia, o código fica simples e
correto, e o custo é copiar 4 KB por escrita.

**Por que os executores recebem o pager a cada `next()`?** Se cada executor
guardasse `&mut Pager`, dois executores (os dois lados de um JOIN, por
exemplo) precisariam de referências mutáveis ao mesmo tempo, o que o Rust
proíbe. A alternativa usual seria `Rc<RefCell<Pager>>`. Passar o pager como
argumento deixa claro quem usa o quê, sem custo em tempo de execução.

**Por que o catálogo guarda o SQL do CREATE?** Para não precisar de outro
formato de serialização. O parser, que já existe, reconstrói as definições.
O SQLite faz exatamente isso.

**Por que a raiz da B+Tree nunca muda de página?** Quando a raiz se divide,
o conteúdo dela vai para uma página nova e a página da raiz vira o novo nó
interno. Assim o catálogo, que aponta para a raiz, nunca precisa ser
atualizado.

**Por que o UPDATE é "remove + insere"?** É a forma mais simples de manter
heap e índices consistentes. A linha muda de `RecordId`, então todos os
índices são atualizados, mesmo os de colunas que não mudaram.
