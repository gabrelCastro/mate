//! A fachada do banco: recebe SQL e devolve resultados.
//!
//! É aqui que todas as camadas se encontram. O caminho de um comando:
//!
//! ```text
//!   "SELECT ..."  ──► sql::parse ──► AST
//!                                     │
//!          ┌──────────────────────────┴──────────────┐
//!          │ DDL (CREATE/DROP)                       │ SELECT/INSERT/UPDATE/DELETE
//!          ▼                                         ▼
//!     catalog.create_table(...)          planner::plan_statement ──► Plan
//!                                                    │
//!                                                    ▼
//!                                       execution::execute ──► linhas
//!          │                                         │
//!          └──────────────► pager.commit() ◄─────────┘
//!                     (se não houver BEGIN aberto)
//! ```
//!
//! # Transações
//!
//! - **Autocommit**: sem BEGIN, cada comando é sua própria transação. Se
//!   der certo, é confirmado. Se der erro, é desfeito por inteiro: um
//!   INSERT de 10 linhas que falha na 7ª não deixa as 6 primeiras.
//! - **BEGIN ... COMMIT**: as alterações se acumulam no pager e só vão para
//!   o disco no COMMIT. ROLLBACK descarta tudo.
//! - **Erro dentro de uma transação**: a transação inteira é desfeita.
//!   (O Postgres marca a transação como abortada e exige ROLLBACK. Bancos
//!   mais completos oferecem SAVEPOINTs para desfazer só um pedaço.)
//!
//! # Concorrência
//!
//! Não há. Um `Database` atende um cliente por vez (é preciso `&mut self`
//! para executar), e só um processo pode abrir o arquivo: os outros
//! recebem `DbError::Busy` (veja a trava em `storage/disk.rs`). Num banco
//! real, a concorrência é uma das partes mais difíceis: locks, MVCC, níveis
//! de isolamento... (veja `docs/exercicios.md`).

use std::path::Path;

use crate::catalog::Catalog;
use crate::error::{DbError, Result};
use crate::execution;
use crate::planner::{Plan, plan_statement};
use crate::result::QueryResult;
use crate::sql::ast::Statement;
use crate::sql::{parse, parse_one};
use crate::storage::pager::{Pager, PagerStats};
use crate::types::Value;

pub struct Database {
    pager: Pager,
    catalog: Catalog,
    in_transaction: bool,
}

impl Database {
    /// Abre o banco no arquivo indicado, criando-o se não existir.
    pub fn open(path: impl AsRef<Path>) -> Result<Database> {
        let mut pager = Pager::open(path.as_ref())?;
        let catalog = Catalog::load(&mut pager)?;
        // Num banco novo, `load` criou a página do catálogo. Confirmamos.
        pager.commit()?;
        Ok(Database {
            pager,
            catalog,
            in_transaction: false,
        })
    }

    /// Executa um ou mais comandos separados por `;` e devolve um resultado
    /// por comando. Para no primeiro erro.
    pub fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        parse(sql)?
            .iter()
            .map(|stmt| self.execute_statement(stmt))
            .collect()
    }

    /// Executa exatamente um comando.
    pub fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.execute_statement(&parse_one(sql)?)
    }

    pub fn execute_statement(&mut self, stmt: &Statement) -> Result<QueryResult> {
        match stmt {
            Statement::Begin => {
                if self.in_transaction {
                    return Err(DbError::Transaction(
                        "já existe uma transação em andamento".into(),
                    ));
                }
                self.in_transaction = true;
                Ok(QueryResult::Message("BEGIN".into()))
            }
            Statement::Commit => {
                if !self.in_transaction {
                    return Err(DbError::Transaction("não há transação em andamento".into()));
                }
                self.pager.commit()?;
                self.in_transaction = false;
                Ok(QueryResult::Message("COMMIT".into()))
            }
            Statement::Rollback => {
                if !self.in_transaction {
                    return Err(DbError::Transaction("não há transação em andamento".into()));
                }
                self.rollback()?;
                Ok(QueryResult::Message("ROLLBACK".into()))
            }
            _ => match self.run(stmt) {
                Ok(result) => {
                    if !self.in_transaction {
                        self.pager.commit()?;
                    }
                    Ok(result)
                }
                Err(err) => {
                    let was_in_transaction = self.in_transaction;
                    self.rollback()?;
                    if was_in_transaction {
                        Err(DbError::Transaction(format!("{err}; a transação foi desfeita")))
                    } else {
                        Err(err)
                    }
                }
            },
        }
    }

    /// Descarta as alterações pendentes. O catálogo em memória é recarregado
    /// do disco, porque pode ter mudado (um CREATE TABLE desfeito, por
    /// exemplo).
    fn rollback(&mut self) -> Result<()> {
        self.pager.rollback()?;
        self.catalog = Catalog::load(&mut self.pager)?;
        self.in_transaction = false;
        Ok(())
    }

    fn run(&mut self, stmt: &Statement) -> Result<QueryResult> {
        match stmt {
            Statement::CreateTable(ct) => {
                self.catalog.create_table(&mut self.pager, ct)?;
                Ok(QueryResult::Message(format!("CREATE TABLE {}", ct.name)))
            }
            Statement::CreateIndex(ci) => {
                self.catalog.create_index(&mut self.pager, ci)?;
                Ok(QueryResult::Message(format!("CREATE INDEX {}", ci.name)))
            }
            Statement::DropTable { name, if_exists } => {
                self.catalog.drop_table(&mut self.pager, name, *if_exists)?;
                Ok(QueryResult::Message(format!("DROP TABLE {name}")))
            }
            Statement::Explain(inner) => {
                let plan = plan_statement(inner, &self.catalog)?.plan;
                let rows = plan
                    .to_string()
                    .lines()
                    .map(|line| vec![Value::Text(line.to_string())])
                    .collect();
                Ok(QueryResult::Rows {
                    columns: vec!["plano".into()],
                    rows,
                })
            }
            Statement::Select(_) => {
                let query = plan_statement(stmt, &self.catalog)?;
                let rows = execution::execute(&query.plan, &self.catalog, &mut self.pager)?;
                Ok(QueryResult::Rows {
                    columns: query.columns,
                    rows,
                })
            }
            Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_) => {
                let query = plan_statement(stmt, &self.catalog)?;
                let rows = execution::execute(&query.plan, &self.catalog, &mut self.pager)?;
                let count = match rows.first().and_then(|r| r.first()) {
                    Some(Value::Integer(n)) => *n as u64,
                    _ => 0,
                };
                let verb = match query.plan {
                    Plan::Insert { .. } => "INSERT",
                    Plan::Update { .. } => "UPDATE",
                    _ => "DELETE",
                };
                Ok(QueryResult::Affected { verb, count })
            }
            Statement::Begin | Statement::Commit | Statement::Rollback => {
                unreachable!("tratados em execute_statement")
            }
        }
    }

    // ------------------------------------------------------------------
    // Introspecção (usada pelos comandos especiais do REPL)
    // ------------------------------------------------------------------

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn stats(&self) -> &PagerStats {
        self.pager.stats()
    }

    pub fn page_count(&self) -> u32 {
        self.pager.page_count()
    }

    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    /// Desenho da B+Tree de um índice (comando `.btree`).
    pub fn dump_index(&mut self, index_name: &str) -> Result<String> {
        let btree = self
            .catalog
            .tables()
            .flat_map(|t| &t.indexes)
            .find(|i| i.name == index_name)
            .map(|i| i.btree)
            .ok_or_else(|| DbError::Plan(format!("o índice '{index_name}' não existe")))?;
        btree.dump(&mut self.pager)
    }
}
