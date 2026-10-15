//! Descrições de tabelas e índices em memória.

use crate::btree::BTree;
use crate::heap::TableHeap;
use crate::sql::ast::CreateTable;
use crate::types::DataType;

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub data_type: DataType,
    pub not_null: bool,
    pub unique: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<Column>,
}

impl TableSchema {
    pub fn from_create(ct: &CreateTable) -> TableSchema {
        TableSchema {
            name: ct.name.clone(),
            columns: ct
                .columns
                .iter()
                .map(|c| Column {
                    name: c.name.clone(),
                    data_type: c.data_type,
                    not_null: c.not_null,
                    unique: c.unique,
                    primary_key: c.primary_key,
                })
                .collect(),
        }
    }

    /// Posição da coluna na linha (as linhas são vetores de valores na
    /// ordem das colunas).
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }
}

/// Um índice: B+Tree sobre uma coluna de uma tabela.
#[derive(Debug, Clone)]
pub struct IndexInfo {
    pub name: String,
    pub table: String,
    pub column: String,
    /// Posição da coluna indexada na linha.
    pub column_idx: usize,
    pub unique: bool,
    pub btree: BTree,
}

/// Tudo o que o banco sabe sobre uma tabela: colunas, onde estão as linhas
/// (heap) e quais índices existem.
#[derive(Debug, Clone)]
pub struct TableInfo {
    pub schema: TableSchema,
    pub heap: TableHeap,
    pub indexes: Vec<IndexInfo>,
}

impl TableInfo {
    pub fn name(&self) -> &str {
        &self.schema.name
    }

    /// O índice sobre a coluna `column_idx`, se houver.
    pub fn index_on(&self, column_idx: usize) -> Option<&IndexInfo> {
        self.indexes.iter().find(|i| i.column_idx == column_idx)
    }
}
