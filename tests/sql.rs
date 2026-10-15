//! Testes de ponta a ponta: SQL entra, linhas saem.
//!
//! Cada teste também serve de exemplo do que o banco sabe fazer.

use mate::test_util::TempDir;
use mate::{Database, DbError, QueryResult};

/// Abre um banco novo num diretório temporário.
fn open(name: &str) -> (TempDir, Database) {
    let dir = TempDir::new(name);
    let db = Database::open(dir.path("test.db")).unwrap();
    (dir, db)
}

/// Executa vários comandos, entrando em pânico se algum falhar.
fn run(db: &mut Database, sql: &str) {
    if let Err(e) = db.execute(sql) {
        panic!("falhou: {sql}\n{e}");
    }
}

/// Executa uma consulta e devolve cada linha como "v1|v2|v3".
fn query(db: &mut Database, sql: &str) -> Vec<String> {
    let result = db.query(sql).unwrap_or_else(|e| panic!("falhou: {sql}\n{e}"));
    result
        .rows()
        .iter()
        .map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("|"))
        .collect()
}

fn users(db: &mut Database) {
    run(
        db,
        "CREATE TABLE users (id INT PRIMARY KEY, nome TEXT NOT NULL, idade INT, cidade TEXT);
         INSERT INTO users VALUES
           (1, 'Ana', 30, 'Recife'),
           (2, 'Bruno', 17, 'Natal'),
           (3, 'Carla', 25, 'Recife'),
           (4, 'Davi', NULL, 'Natal'),
           (5, 'Eva', 41, NULL);",
    );
}

#[test]
fn select_basico() {
    let (_dir, mut db) = open("select");
    users(&mut db);
    assert_eq!(
        query(&mut db, "SELECT nome FROM users WHERE id = 3"),
        vec!["Carla"]
    );
    assert_eq!(
        query(
            &mut db,
            "SELECT nome, idade + 1 FROM users WHERE idade > 20 ORDER BY idade"
        ),
        vec!["Carla|26", "Ana|31", "Eva|42"]
    );
    assert_eq!(
        query(&mut db, "SELECT 1 + 2 * 3, 'a' || 'b', 7 / 2, 7.0 / 2"),
        vec!["7|ab|3|3.5"]
    );
}

#[test]
fn logica_de_tres_valores_no_where() {
    let (_dir, mut db) = open("nulls");
    users(&mut db);
    // Davi tem idade NULL: não é > 20 nem <= 20.
    assert_eq!(
        query(&mut db, "SELECT COUNT(*) FROM users WHERE idade > 20"),
        vec!["3"]
    );
    assert_eq!(
        query(&mut db, "SELECT COUNT(*) FROM users WHERE idade <= 20"),
        vec!["1"]
    );
    assert_eq!(
        query(&mut db, "SELECT nome FROM users WHERE idade IS NULL"),
        vec!["Davi"]
    );
    assert_eq!(
        query(&mut db, "SELECT COUNT(*) FROM users WHERE idade = NULL"),
        vec!["0"]
    );
    assert_eq!(
        query(
            &mut db,
            "SELECT nome FROM users WHERE cidade IN ('Natal', 'X') ORDER BY nome"
        ),
        vec!["Bruno", "Davi"]
    );
    assert_eq!(
        query(
            &mut db,
            "SELECT nome FROM users WHERE idade BETWEEN 20 AND 35 ORDER BY 1"
        ),
        vec!["Ana", "Carla"]
    );
    assert_eq!(
        query(
            &mut db,
            "SELECT COALESCE(cidade, '?'), UPPER(nome) FROM users WHERE id = 5"
        ),
        vec!["?|EVA"]
    );
}

#[test]
fn agregacao() {
    let (_dir, mut db) = open("agg");
    users(&mut db);
    assert_eq!(
        query(
            &mut db,
            "SELECT COUNT(*), COUNT(idade), SUM(idade), MIN(nome), MAX(idade) FROM users"
        ),
        vec!["5|4|113|Ana|41"]
    );
    assert_eq!(
        query(
            &mut db,
            "SELECT cidade, COUNT(*) AS n FROM users GROUP BY cidade HAVING COUNT(*) > 1 ORDER BY n DESC, cidade"
        ),
        vec!["Natal|2", "Recife|2"]
    );
    // Sem GROUP BY, tabela vazia ainda produz uma linha.
    run(&mut db, "CREATE TABLE vazia (x INT)");
    assert_eq!(
        query(&mut db, "SELECT COUNT(*), SUM(x) FROM vazia"),
        vec!["0|NULL"]
    );
    // Coluna fora do GROUP BY é erro.
    assert!(matches!(
        db.query("SELECT nome, COUNT(*) FROM users GROUP BY cidade"),
        Err(DbError::Plan(_))
    ));
}

#[test]
fn joins() {
    let (_dir, mut db) = open("joins");
    users(&mut db);
    run(
        &mut db,
        "CREATE TABLE pedidos (id INT PRIMARY KEY, user_id INT, valor FLOAT);
         INSERT INTO pedidos VALUES (10, 1, 100), (11, 1, 50.5), (12, 3, 20);",
    );
    assert_eq!(
        query(
            &mut db,
            "SELECT u.nome, p.valor FROM users u JOIN pedidos p ON p.user_id = u.id ORDER BY p.valor"
        ),
        vec!["Carla|20.0", "Ana|50.5", "Ana|100.0"]
    );
    assert_eq!(
        query(
            &mut db,
            "SELECT u.nome, COUNT(p.id) FROM users u LEFT JOIN pedidos p ON p.user_id = u.id
             GROUP BY u.nome ORDER BY 2 DESC, 1"
        ),
        vec!["Ana|2", "Carla|1", "Bruno|0", "Davi|0", "Eva|0"]
    );
    // `id` existe nas duas tabelas.
    assert!(matches!(
        db.query("SELECT id FROM users u JOIN pedidos p ON p.user_id = u.id"),
        Err(DbError::Plan(_))
    ));
}

#[test]
fn ordenacao_distinct_limit() {
    let (_dir, mut db) = open("order");
    users(&mut db);
    assert_eq!(
        query(&mut db, "SELECT DISTINCT cidade FROM users ORDER BY cidade"),
        vec!["NULL", "Natal", "Recife"]
    );
    assert_eq!(
        query(
            &mut db,
            "SELECT nome FROM users ORDER BY id DESC LIMIT 2 OFFSET 1"
        ),
        vec!["Davi", "Carla"]
    );
    assert_eq!(
        query(&mut db, "SELECT nome AS n FROM users ORDER BY n LIMIT 1"),
        vec!["Ana"]
    );
}

#[test]
fn restricoes_e_atomicidade() {
    let (_dir, mut db) = open("constraints");
    users(&mut db);
    let err = |db: &mut Database, sql: &str| db.query(sql).unwrap_err();
    assert!(matches!(
        err(&mut db, "INSERT INTO users VALUES (1, 'X', 1, 'Y')"),
        DbError::Constraint(_)
    ));
    assert!(matches!(
        err(&mut db, "INSERT INTO users (id) VALUES (9)"),
        DbError::Constraint(_)
    ));
    assert!(matches!(
        err(&mut db, "INSERT INTO users VALUES (9, 'X', 'velho', 'Y')"),
        DbError::Execution(_)
    ));
    assert!(matches!(
        err(&mut db, "UPDATE users SET id = 2 WHERE id = 1"),
        DbError::Constraint(_)
    ));

    // Um INSERT de várias linhas que falha no meio não deixa nenhuma.
    assert!(
        db.query("INSERT INTO users VALUES (6, 'F', 1, 'X'), (7, 'G', 2, 'X'), (1, 'dup', 3, 'X')")
            .is_err()
    );
    assert_eq!(query(&mut db, "SELECT COUNT(*) FROM users"), vec!["5"]);
    assert!(query(&mut db, "SELECT * FROM users WHERE id = 6").is_empty());
}

#[test]
fn transacoes() {
    let (_dir, mut db) = open("txn");
    users(&mut db);

    run(
        &mut db,
        "BEGIN; DELETE FROM users WHERE cidade = 'Natal'; CREATE TABLE temp (x INT);",
    );
    assert_eq!(query(&mut db, "SELECT COUNT(*) FROM users"), vec!["3"]);
    run(&mut db, "ROLLBACK");
    assert_eq!(query(&mut db, "SELECT COUNT(*) FROM users"), vec!["5"]);
    assert!(
        db.query("SELECT * FROM temp").is_err(),
        "CREATE TABLE deveria ter sido desfeito"
    );

    // Erro no meio de uma transação desfaz a transação inteira.
    run(&mut db, "BEGIN; INSERT INTO users VALUES (6, 'F', 1, 'X')");
    assert!(matches!(
        db.query("INSERT INTO users VALUES (6, 'F', 1, 'X')"),
        Err(DbError::Transaction(_))
    ));
    assert!(!db.in_transaction());
    assert_eq!(query(&mut db, "SELECT COUNT(*) FROM users"), vec!["5"]);

    run(&mut db, "BEGIN; UPDATE users SET idade = 0; COMMIT;");
    assert_eq!(query(&mut db, "SELECT SUM(idade) FROM users"), vec!["0"]);
    assert!(db.query("COMMIT").is_err());
}

#[test]
fn persistencia_entre_aberturas() {
    let dir = TempDir::new("persist");
    let path = dir.path("p.db");
    {
        let mut db = Database::open(&path).unwrap();
        users(&mut db);
        run(&mut db, "CREATE INDEX idx_idade ON users (idade)");
        run(&mut db, "BEGIN; INSERT INTO users VALUES (99, 'Perdido', 1, 'X')");
        // Fecha o banco com a transação aberta: ela deve ser descartada.
    }
    let mut db = Database::open(&path).unwrap();
    assert_eq!(query(&mut db, "SELECT COUNT(*) FROM users"), vec!["5"]);
    assert_eq!(
        query(&mut db, "SELECT nome FROM users WHERE idade = 25"),
        vec!["Carla"]
    );
    let plan = query(&mut db, "EXPLAIN SELECT nome FROM users WHERE idade = 25");
    assert!(
        plan.iter()
            .any(|l| l.contains("IndexScan") && l.contains("idx_idade"))
    );
}

/// Muitas linhas: força splits na B+Tree e várias páginas no heap, e
/// confere que IndexScan e SeqScan concordam.
#[test]
fn muitas_linhas_indice_e_scan_concordam() {
    let dir = TempDir::new("many");
    let path = dir.path("m.db");
    {
        let mut db = Database::open(&path).unwrap();
        run(
            &mut db,
            "CREATE TABLE t (id INT PRIMARY KEY, grupo INT, texto TEXT)",
        );
        run(&mut db, "CREATE INDEX t_grupo ON t (grupo)");
        let mut sql = String::from("BEGIN;");
        for i in 0..3000 {
            sql.push_str(&format!(
                "INSERT INTO t VALUES ({}, {}, 'linha número {i} com algum texto');",
                (i * 7919) % 3000,
                i % 10
            ));
        }
        sql.push_str("COMMIT;");
        run(&mut db, &sql);
        run(&mut db, "DELETE FROM t WHERE grupo = 3");
        run(&mut db, "UPDATE t SET grupo = 3 WHERE grupo = 4");
    }

    let mut db = Database::open(&path).unwrap();
    assert_eq!(query(&mut db, "SELECT COUNT(*) FROM t"), vec!["2700"]);

    // Pelo índice (id é PRIMARY KEY)...
    let via_index = query(&mut db, "SELECT id FROM t WHERE id >= 2990");
    let plan = query(&mut db, "EXPLAIN SELECT id FROM t WHERE id >= 2990");
    assert!(plan.iter().any(|l| l.contains("IndexScan")));
    // ...e sem índice (a expressão `id + 0` impede o uso do índice).
    let via_scan = query(&mut db, "SELECT id FROM t WHERE id + 0 >= 2990 ORDER BY id");
    assert_eq!(via_index, via_scan);
    // Dos ids 2990..2999, os que eram do grupo 3 foram apagados.
    let deleted = (0..3000)
        .filter(|i| (i * 7919) % 3000 >= 2990 && i % 10 == 3)
        .count();
    assert_eq!(via_index.len(), 10 - deleted);

    // O índice secundário reflete o DELETE e o UPDATE.
    assert_eq!(
        query(&mut db, "SELECT COUNT(*) FROM t WHERE grupo = 3"),
        vec!["300"]
    );
    assert_eq!(
        query(&mut db, "SELECT COUNT(*) FROM t WHERE grupo = 4"),
        vec!["0"]
    );
}

#[test]
fn drop_table() {
    let (_dir, mut db) = open("drop");
    users(&mut db);
    run(&mut db, "DROP TABLE users");
    assert!(db.query("SELECT * FROM users").is_err());
    run(&mut db, "DROP TABLE IF EXISTS users");
    users(&mut db);
    assert_eq!(query(&mut db, "SELECT COUNT(*) FROM users"), vec!["5"]);
    assert!(db.query("DROP TABLE mate_schema").is_err());
}

#[test]
fn resultado_formatado() {
    let (_dir, mut db) = open("format");
    users(&mut db);
    let result = db.query("SELECT id, nome FROM users WHERE id <= 2").unwrap();
    assert_eq!(
        result.to_string(),
        " id | nome\n----+-------\n 1  | Ana\n 2  | Bruno\n(2 linhas)"
    );
    assert_eq!(
        db.query("DELETE FROM users WHERE id = 1").unwrap(),
        QueryResult::Affected {
            verb: "DELETE",
            count: 1
        }
    );
}
