# Exercícios

Ler código ensina muito, mas **mudar** código ensina mais. Os exercícios
estão em ordem crescente de dificuldade, e cada um indica onde mexer. Rode
`cargo test` depois de cada mudança.

## Aquecimento

1. **Nova função escalar.** Adicione `ROUND(x)` e `SUBSTR(texto, inicio,
   tamanho)`.
   *Onde:* `ScalarFunction` em `execution/expr.rs`.

2. **Valor padrão.** Suporte `CREATE TABLE t (x INT DEFAULT 0)`. Colunas
   omitidas no INSERT recebem o padrão em vez de NULL.
   *Onde:* `ColumnDef` (ast), `parse_column_def` (parser), `Column` (catalog),
   `plan_insert` (planner). Lembre que o `Display` do `CreateTable` precisa
   gerar o DEFAULT, senão ele se perde ao reabrir o banco.

3. **`.count` no REPL.** Um comando que mostra quantas páginas cada tabela
   ocupa.
   *Onde:* `main.rs` e `HeapCursor`/`SlottedPage::next_page`.

## Intermediário

4. **Free list.** Hoje `DROP TABLE` vaza páginas. Mantenha uma lista
   encadeada de páginas livres a partir do cabeçalho (página 0), e faça
   `Pager::allocate` reusá-las. Atenção: a lista precisa ser transacional,
   como o resto.

5. **Merge na B+Tree.** Quando uma folha fica com menos da metade do espaço
   ocupado após um `delete`, peça entradas emprestadas a um irmão ou funda as
   duas folhas, e atualize o pai. É o exercício clássico de B+Tree.
   *Onde:* `btree/tree.rs`.

6. **CLOCK em vez de LRU.** Substitua a busca linear do `LruCache` pelo
   algoritmo CLOCK (um bit de "referenciada" por página e um ponteiro que
   gira). Compare o `.stats` dos dois numa carga de trabalho grande.

7. **Atualização no lugar.** Se a nova versão da linha cabe no slot antigo,
   grave no mesmo lugar (mesmo `RecordId`) e só atualize os índices das
   colunas que mudaram.
   *Onde:* `update_row` em `execution/modify.rs` e um `update` novo em
   `SlottedPage`.

8. **Índices compostos.** `CREATE INDEX i ON t (a, b)`. A chave da B+Tree
   passa a ser um vetor de valores. Ajuste o planner para usar o índice
   quando o WHERE tiver igualdade em `a` (e opcionalmente em `b`).

## Avançado

9. **Hash join.** Para `ON a.x = b.y`, monte uma `HashMap<Value, Vec<linha>>`
   com o lado direito e consulte com cada linha da esquerda. Faça o planner
   escolher hash join quando o ON for uma igualdade entre colunas.
   *Onde:* novo `execution/hash_join.rs` e `plan_joins` no planner.

10. **Index nested loop join.** Se a coluna da direita no ON tem índice,
    busque nele para cada linha da esquerda em vez de percorrer a tabela
    toda.

11. **Estatísticas e otimizador por custo.** Implemente `ANALYZE t`, que
    guarda no catálogo o número de linhas e de valores distintos por coluna.
    Use isso para decidir entre SeqScan e IndexScan: um intervalo que cobre
    80% da tabela é mais barato por SeqScan.

12. **Linhas grandes (overflow).** Hoje uma linha precisa caber numa página.
    Guarde textos longos em páginas de overflow encadeadas, deixando na linha
    só um ponteiro (o Postgres chama isso de TOAST).

13. **SAVEPOINT.** `SAVEPOINT s; ...; ROLLBACK TO s;`. Dica: o pager pode
    tirar uma "foto" do conjunto `dirty` e do `page_count`.

## Desafios grandes

14. **Concorrência com MVCC.** Permita várias conexões. Cada linha ganha
    `xmin`/`xmax` (a transação que a criou e a que a apagou), e cada
    transação enxerga só as versões visíveis no seu *snapshot*. É o coração
    do Postgres. Comece lendo sobre *snapshot isolation*.

15. **Protocolo de rede do Postgres.** Implemente o *wire protocol* do
    Postgres (mensagens `Query`, `RowDescription`, `DataRow`...) sobre TCP.
    Assim você conecta no mate com o `psql`.

16. **Steal + undo (ARIES).** Deixe o pager gravar no disco páginas de
    transações em andamento quando o cache enche. Para isso o WAL precisa
    registrar como **desfazer** cada mudança, e a recuperação ganha as fases
    *analysis*, *redo* e *undo*.

## Para ir além

- **CMU 15-445** (Andy Pavlo), aulas gratuitas no YouTube. Os tópicos
  seguem quase a mesma ordem das camadas deste projeto.
- **Database Internals**, de Alex Petrov: storage e sistemas distribuídos.
- **Architecture of a Database System**, de Hellerstein, Stonebraker e
  Hamilton: artigo curto e excelente com a visão geral.
- **SQLite**: o arquivo `btree.c` e a página "SQLite File Format" são ótimas
  leituras depois de entender este projeto.
