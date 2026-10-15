//! # Catálogo: quais tabelas e índices existem
//!
//! 1. [`schema`] — as structs em memória (colunas, tabelas, índices).
//! 2. [`catalog`] — como essas definições são guardadas na tabela de sistema
//!    `mate_schema` e recarregadas ao abrir o banco.

#[allow(clippy::module_inception)]
pub mod catalog;
pub mod schema;

pub use catalog::{CATALOG_TABLE, Catalog};
pub use schema::{Column, IndexInfo, TableInfo, TableSchema};
