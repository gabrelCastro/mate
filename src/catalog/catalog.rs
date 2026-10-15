//! O catálogo: o "banco de dados sobre o banco de dados".
//!
//! Quando você roda `CREATE TABLE users (...)`, o banco precisa lembrar que
//! a tabela existe, quais são as colunas e em qual página começam as
//! linhas, inclusive depois de fechar e reabrir o arquivo.
//!
//! # O catálogo é uma tabela
//!
//! A ideia, a mesma do `sqlite_master` do SQLite, é guardar o catálogo
//! **numa tabela comum**, a `mate_schema`, cujo heap começa sempre na
//! página 1:
//!
//! | type    | name          | tbl_name | root | sql                                   |
//! |---------|---------------|----------|------|---------------------------------------|
//! | table   | users         | users    | 2    | CREATE TABLE users (id INTEGER ...)   |
//! | index   | users_pkey    | users    | 3    | CREATE UNIQUE INDEX users_pkey ...    |
//!
//! Ao abrir o banco, lemos essas linhas e fazemos o parse do `sql` de cada
//! uma para reconstruir as definições. Assim reaproveitamos o heap, a
//! serialização e o parser, sem precisar de um formato especial para
//! metadados. E você pode consultar o catálogo com SQL:
//! `SELECT * FROM mate_schema;`
//!
//! Como o catálogo vive em páginas comuns, criar ou remover tabelas também
//! é transacional: um `ROLLBACK` desfaz um `CREATE TABLE`.

use std::collections::BTreeMap;

use crate::btree::{BTree, IndexEntry};
use crate::catalog::schema::{Column, IndexInfo, TableInfo, TableSchema};
use crate::error::{DbError, Result};
use crate::heap::TableHeap;
use crate::sql::ast::{CreateIndex, CreateTable, Statement};
use crate::sql::parse_one;
use crate::storage::page::PageId;
use crate::storage::pager::Pager;
use crate::types::encoding::{decode_row, encode_row};
use crate::types::{DataType, Value};

pub const CATALOG_TABLE: &str = "mate_schema";
/// A página fixa onde começa o heap do catálogo.
pub const CATALOG_ROOT: PageId = 1;

pub struct Catalog {
    tables: BTreeMap<String, TableInfo>,
}

impl Catalog {
    /// Lê o catálogo do disco. Num banco novo, cria antes o heap do catálogo.
    pub fn load(pager: &mut Pager) -> Result<Catalog> {
        if pager.page_count() == 1 {
            // Só existe o cabeçalho: banco recém-criado.
            let heap = TableHeap::create(pager)?;
            assert_eq!(heap.first_page, CATALOG_ROOT);
        }

        let mut catalog = Catalog {
            tables: BTreeMap::new(),
        };
        catalog.tables.insert(CATALOG_TABLE.to_string(), system_table());

        // Lê todas as linhas do catálogo.
        let mut tables = Vec::new();
        let mut indexes = Vec::new();
        let mut cursor = TableHeap::open(CATALOG_ROOT).scan();
        while let Some((_, bytes)) = cursor.next(pager)? {
            let row = CatalogRow::from_values(decode_row(&bytes)?)?;
            match row.kind.as_str() {
                "table" => tables.push(row),
                "index" => indexes.push(row),
                other => {
                    return Err(DbError::Corrupt(format!(
                        "tipo desconhecido no catálogo: {other}"
                    )));
                }
            }
        }

        // Tabelas primeiro, porque os índices precisam delas.
        for row in tables {
            let Statement::CreateTable(ct) = parse_one(&row.sql)? else {
                return Err(DbError::Corrupt(format!("SQL inválido no catálogo: {}", row.sql)));
            };
            let info = TableInfo {
                schema: TableSchema::from_create(&ct),
                heap: TableHeap::open(row.root),
                indexes: Vec::new(),
            };
            catalog.tables.insert(row.name, info);
        }
        for row in indexes {
            let Statement::CreateIndex(ci) = parse_one(&row.sql)? else {
                return Err(DbError::Corrupt(format!("SQL inválido no catálogo: {}", row.sql)));
            };
            let index = catalog.index_info(&ci, BTree::open(row.root))?;
            catalog.tables.get_mut(&ci.table).unwrap().indexes.push(index);
        }
        Ok(catalog)
    }

    /// Busca uma tabela ou devolve erro "não existe".
    pub fn table(&self, name: &str) -> Result<&TableInfo> {
        self.tables
            .get(name)
            .ok_or_else(|| DbError::Plan(format!("a tabela '{name}' não existe")))
    }

    pub fn tables(&self) -> impl Iterator<Item = &TableInfo> {
        self.tables.values()
    }

    pub fn create_table(&mut self, pager: &mut Pager, ct: &CreateTable) -> Result<()> {
        if self.tables.contains_key(&ct.name) {
            if ct.if_not_exists {
                return Ok(());
            }
            return Err(DbError::Plan(format!("a tabela '{}' já existe", ct.name)));
        }
        for (i, col) in ct.columns.iter().enumerate() {
            if ct.columns[..i].iter().any(|c| c.name == col.name) {
                return Err(DbError::Plan(format!("coluna '{}' repetida", col.name)));
            }
        }
        if ct.columns.iter().filter(|c| c.primary_key).count() > 1 {
            return Err(DbError::Plan("só uma coluna pode ser PRIMARY KEY".into()));
        }

        let heap = TableHeap::create(pager)?;
        self.insert_catalog_row(
            pager,
            "table",
            &ct.name,
            &ct.name,
            heap.first_page,
            &ct.to_string(),
        )?;
        self.tables.insert(
            ct.name.clone(),
            TableInfo {
                schema: TableSchema::from_create(ct),
                heap,
                indexes: Vec::new(),
            },
        );

        // PRIMARY KEY e UNIQUE são garantidos por índices únicos criados
        // automaticamente (o Postgres faz igual, com os mesmos nomes).
        for col in ct.columns.iter().filter(|c| c.unique) {
            let suffix = if col.primary_key { "pkey" } else { "key" };
            let name = if col.primary_key {
                format!("{}_{suffix}", ct.name)
            } else {
                format!("{}_{}_{suffix}", ct.name, col.name)
            };
            self.create_index(
                pager,
                &CreateIndex {
                    name,
                    table: ct.name.clone(),
                    column: col.name.clone(),
                    unique: true,
                },
            )?;
        }
        Ok(())
    }

    /// Cria um índice e o preenche com as linhas que já existem na tabela.
    pub fn create_index(&mut self, pager: &mut Pager, ci: &CreateIndex) -> Result<()> {
        if ci.table == CATALOG_TABLE {
            return Err(DbError::Plan("não é possível indexar a tabela do sistema".into()));
        }
        if self.tables().flat_map(|t| &t.indexes).any(|i| i.name == ci.name) {
            return Err(DbError::Plan(format!("o índice '{}' já existe", ci.name)));
        }
        let btree = BTree::create(pager)?;
        let index = self.index_info(ci, btree)?;
        if self.table(&ci.table)?.index_on(index.column_idx).is_some() {
            return Err(DbError::Plan(format!(
                "a coluna '{}' já tem um índice",
                ci.column
            )));
        }

        // Preenche o índice com as linhas existentes (*backfill*).
        let mut cursor = self.table(&ci.table)?.heap.scan();
        while let Some((rid, bytes)) = cursor.next(pager)? {
            let key = decode_row(&bytes)?.swap_remove(index.column_idx);
            if index.unique && !key.is_null() && btree.contains_key(pager, &key)? {
                return Err(DbError::Constraint(format!(
                    "não é possível criar o índice único '{}': o valor {key} se repete",
                    ci.name
                )));
            }
            btree.insert(pager, IndexEntry { key, rid })?;
        }

        self.insert_catalog_row(pager, "index", &ci.name, &ci.table, btree.root, &ci.to_string())?;
        self.tables.get_mut(&ci.table).unwrap().indexes.push(index);
        Ok(())
    }

    /// Remove a tabela (e seus índices) do catálogo.
    ///
    /// Simplificação: as páginas da tabela não são liberadas, porque não
    /// temos *free list* (veja `Pager::allocate`).
    pub fn drop_table(&mut self, pager: &mut Pager, name: &str, if_exists: bool) -> Result<()> {
        if name == CATALOG_TABLE {
            return Err(DbError::Plan("não é possível remover a tabela do sistema".into()));
        }
        if !self.tables.contains_key(name) {
            if if_exists {
                return Ok(());
            }
            return Err(DbError::Plan(format!("a tabela '{name}' não existe")));
        }
        // Apaga do heap do catálogo todas as linhas com tbl_name = name.
        let heap = TableHeap::open(CATALOG_ROOT);
        let mut cursor = heap.scan();
        let mut to_delete = Vec::new();
        while let Some((rid, bytes)) = cursor.next(pager)? {
            if CatalogRow::from_values(decode_row(&bytes)?)?.table == name {
                to_delete.push(rid);
            }
        }
        for rid in to_delete {
            heap.delete(pager, rid)?;
        }
        self.tables.remove(name);
        Ok(())
    }

    fn index_info(&self, ci: &CreateIndex, btree: BTree) -> Result<IndexInfo> {
        let table = self.table(&ci.table)?;
        let column_idx = table
            .schema
            .column_index(&ci.column)
            .ok_or_else(|| DbError::Plan(format!("a coluna '{}' não existe em '{}'", ci.column, ci.table)))?;
        Ok(IndexInfo {
            name: ci.name.clone(),
            table: ci.table.clone(),
            column: ci.column.clone(),
            column_idx,
            unique: ci.unique,
            btree,
        })
    }

    fn insert_catalog_row(
        &self,
        pager: &mut Pager,
        kind: &str,
        name: &str,
        table: &str,
        root: PageId,
        sql: &str,
    ) -> Result<()> {
        let row = [
            Value::Text(kind.into()),
            Value::Text(name.into()),
            Value::Text(table.into()),
            Value::Integer(root as i64),
            Value::Text(sql.into()),
        ];
        TableHeap::open(CATALOG_ROOT).insert(pager, &encode_row(&row))?;
        Ok(())
    }
}

/// Uma linha da `mate_schema`, já com os tipos certos.
struct CatalogRow {
    kind: String,
    name: String,
    table: String,
    root: PageId,
    sql: String,
}

impl CatalogRow {
    fn from_values(values: Vec<Value>) -> Result<CatalogRow> {
        match <[Value; 5]>::try_from(values) {
            Ok(
                [
                    Value::Text(kind),
                    Value::Text(name),
                    Value::Text(table),
                    Value::Integer(root),
                    Value::Text(sql),
                ],
            ) => Ok(CatalogRow {
                kind,
                name,
                table,
                root: root as PageId,
                sql,
            }),
            _ => Err(DbError::Corrupt("linha do catálogo em formato inesperado".into())),
        }
    }
}

/// A definição da própria `mate_schema`, para que ela possa ser
/// consultada com SELECT como qualquer tabela.
fn system_table() -> TableInfo {
    let col = |name: &str, data_type| Column {
        name: name.into(),
        data_type,
        not_null: true,
        unique: false,
        primary_key: false,
    };
    TableInfo {
        schema: TableSchema {
            name: CATALOG_TABLE.into(),
            columns: vec![
                col("type", DataType::Text),
                col("name", DataType::Text),
                col("tbl_name", DataType::Text),
                col("root", DataType::Integer),
                col("sql", DataType::Text),
            ],
        },
        heap: TableHeap::open(CATALOG_ROOT),
        indexes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    #[test]
    fn catalogo_sobrevive_a_reabertura() {
        let dir = TempDir::new("catalog");
        let path = dir.path("t.db");
        {
            let mut pager = Pager::open(&path).unwrap();
            let mut catalog = Catalog::load(&mut pager).unwrap();
            let Statement::CreateTable(ct) =
                parse_one("CREATE TABLE users (id INT PRIMARY KEY, email TEXT UNIQUE, nome TEXT)").unwrap()
            else {
                panic!()
            };
            catalog.create_table(&mut pager, &ct).unwrap();
            pager.commit().unwrap();
        }
        let mut pager = Pager::open(&path).unwrap();
        let catalog = Catalog::load(&mut pager).unwrap();
        let users = catalog.table("users").unwrap();
        assert_eq!(users.schema.columns.len(), 3);
        let names: Vec<&str> = users.indexes.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["users_pkey", "users_email_key"]);
        assert!(catalog.table("nada").is_err());
    }
}
