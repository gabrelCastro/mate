//! # Mate: um SGBD relacional didático
//!
//! Este crate é um banco de dados relacional completo, mas pequeno, escrito
//! para ser **lido**. Cada módulo começa com um comentário explicando o
//! conceito que implementa, e o código prefere clareza a desempenho.
//!
//! ## Arquitetura em camadas
//!
//! ```text
//!   ┌───────────────────────────────────────────────────────────┐
//!   │  main.rs (REPL)                                            │
//!   ├───────────────────────────────────────────────────────────┤
//!   │  database      fachada: SQL entra, resultado sai; transações│
//!   ├──────────────┬──────────────┬─────────────────────────────┤
//!   │  sql         │  planner     │  execution                   │
//!   │  lexer       │  binding     │  executores Volcano          │
//!   │  parser→AST  │  otimizador  │  (scan, filter, join, sort…)│
//!   ├──────────────┴──────────────┴─────────────────────────────┤
//!   │  catalog       quais tabelas/índices existem               │
//!   ├──────────────────────────────┬────────────────────────────┤
//!   │  heap (linhas)               │  btree (índices)            │
//!   ├──────────────────────────────┴────────────────────────────┤
//!   │  types         Value e serialização em bytes               │
//!   ├───────────────────────────────────────────────────────────┤
//!   │  storage       páginas, cache, WAL, pager                  │
//!   └───────────────────────────────────────────────────────────┘
//!                         arquivo.db + arquivo.db-wal
//! ```
//!
//! Cada camada só conhece as que estão **abaixo** dela.
//!
//! ## Ordem de leitura sugerida
//!
//! De baixo para cima: cada camada se apoia só no que você já leu.
//!
//! 1. [`storage`] — páginas, disco, cache, WAL e pager.
//! 2. [`types`] — valores SQL e como viram bytes.
//! 3. [`heap`] — linhas dentro de páginas e tabelas como listas de páginas.
//! 4. [`btree`] — índices.
//! 5. [`catalog`] — o dicionário de dados.
//! 6. [`sql`] — lexer, AST e parser.
//! 7. [`planner`] — AST → plano.
//! 8. [`execution`] — rodando o plano.
//! 9. [`database`] — juntando tudo, com transações.

pub mod btree;
pub mod catalog;
pub mod database;
pub mod error;
pub mod execution;
pub mod heap;
pub mod planner;
pub mod result;
pub mod sql;
pub mod storage;
#[doc(hidden)]
pub mod test_util;
pub mod types;

pub use database::Database;
pub use error::{DbError, Result};
pub use result::QueryResult;
pub use types::Value;
