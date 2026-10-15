//! INSERT, UPDATE e DELETE: alterar linhas **e manter os índices em dia**.
//!
//! Toda alteração numa linha precisa ser refletida em cada índice da
//! tabela. Senão o índice passa a apontar para linhas que não existem, ou
//! deixa de achar linhas que existem. As três funções `insert_row`,
//! `delete_row` e `update_row` concentram essa responsabilidade.
//!
//! # O problema do Halloween
//!
//! Imagine `UPDATE func SET salario = salario * 1.1 WHERE salario < 5000`
//! lendo por um índice em `salario`. Cada linha atualizada recebe uma nova
//! entrada no índice, **mais adiante** na ordem, e o próprio scan a
//! encontra de novo e a atualiza outra vez, até passar de 5000. (O problema
//! foi descoberto pela IBM num dia 31 de outubro.)
//!
//! A solução clássica, usada aqui: o UPDATE e o DELETE primeiro **coletam**
//! todas as linhas afetadas e só depois começam a alterar.

use crate::btree::IndexEntry;
use crate::catalog::TableInfo;
use crate::error::{DbError, Result};
use crate::execution::expr::BoundExpr;
use crate::execution::{Executor, Tuple};
use crate::heap::RecordId;
use crate::storage::pager::Pager;
use crate::types::Value;
use crate::types::encoding::encode_row;

/// Valida e insere uma linha: tipos, NOT NULL, UNIQUE, heap e índices.
pub fn insert_row(pager: &mut Pager, table: &TableInfo, values: Vec<Value>) -> Result<RecordId> {
    // 1. Tipos e NOT NULL.
    let mut row = Vec::with_capacity(values.len());
    for (value, col) in values.into_iter().zip(&table.schema.columns) {
        if value.is_null() && col.not_null {
            return Err(DbError::Constraint(format!(
                "a coluna '{}' não pode ser NULL",
                col.name
            )));
        }
        row.push(
            value
                .coerce_to(col.data_type)
                .map_err(|e| DbError::Execution(format!("coluna '{}': {e}", col.name)))?,
        );
    }

    // 2. UNIQUE: consulta cada índice único antes de alterar qualquer coisa.
    for index in table.indexes.iter().filter(|i| i.unique) {
        let key = &row[index.column_idx];
        if !key.is_null() && index.btree.contains_key(pager, key)? {
            return Err(DbError::Constraint(format!(
                "valor duplicado {key} na coluna '{}' (índice único '{}')",
                index.column, index.name
            )));
        }
    }

    // 3. Grava no heap e registra em cada índice.
    let rid = table.heap.insert(pager, &encode_row(&row))?;
    for index in &table.indexes {
        let key = row[index.column_idx].clone();
        index.btree.insert(pager, IndexEntry { key, rid })?;
    }
    Ok(rid)
}

/// Remove uma linha do heap e dos índices. `values` é o conteúdo atual dela
/// (precisamos dele para achar as entradas nos índices).
pub fn delete_row(pager: &mut Pager, table: &TableInfo, rid: RecordId, values: &[Value]) -> Result<()> {
    table.heap.delete(pager, rid)?;
    for index in &table.indexes {
        let entry = IndexEntry {
            key: values[index.column_idx].clone(),
            rid,
        };
        index.btree.delete(pager, &entry)?;
    }
    Ok(())
}

/// UPDATE = remove a versão antiga e insere a nova. A linha muda de
/// `RecordId`, e por isso todos os índices são atualizados. Bancos reais
/// atualizam no lugar quando a linha cabe, e só tocam nos índices das
/// colunas que mudaram (o Postgres chama isso de *HOT update*).
pub fn update_row(
    pager: &mut Pager,
    table: &TableInfo,
    rid: RecordId,
    old: &[Value],
    new: Vec<Value>,
) -> Result<()> {
    delete_row(pager, table, rid, old)?;
    insert_row(pager, table, new)?;
    Ok(())
}

/// Linha única com o número de linhas afetadas, o resultado dos comandos
/// de modificação.
fn count_tuple(n: usize) -> Tuple {
    Tuple::new(vec![Value::Integer(n as i64)])
}

pub struct Insert {
    table: TableInfo,
    rows: Vec<Vec<Value>>,
    done: bool,
}

impl Insert {
    pub fn new(table: TableInfo, rows: Vec<Vec<Value>>) -> Insert {
        Insert {
            table,
            rows,
            done: false,
        }
    }
}

impl Executor for Insert {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        if self.done {
            return Ok(None);
        }
        self.done = true;
        let rows = std::mem::take(&mut self.rows);
        let n = rows.len();
        for row in rows {
            insert_row(pager, &self.table, row)?;
        }
        Ok(Some(count_tuple(n)))
    }
}

/// Lê todas as linhas da entrada antes de alterar qualquer uma (veja o
/// problema do Halloween no topo do arquivo).
fn collect_targets(input: &mut dyn Executor, pager: &mut Pager) -> Result<Vec<(RecordId, Vec<Value>)>> {
    let mut targets = Vec::new();
    while let Some(tuple) = input.next(pager)? {
        let rid = tuple
            .rid
            .ok_or_else(|| DbError::Execution("linha sem RecordId na entrada de UPDATE/DELETE".into()))?;
        targets.push((rid, tuple.values));
    }
    Ok(targets)
}

pub struct Update {
    table: TableInfo,
    input: Box<dyn Executor>,
    assignments: Vec<(usize, BoundExpr)>,
    done: bool,
}

impl Update {
    pub fn new(table: TableInfo, input: Box<dyn Executor>, assignments: Vec<(usize, BoundExpr)>) -> Update {
        Update {
            table,
            input,
            assignments,
            done: false,
        }
    }
}

impl Executor for Update {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        if self.done {
            return Ok(None);
        }
        self.done = true;
        let targets = collect_targets(self.input.as_mut(), pager)?;
        for (rid, old) in &targets {
            // Todas as expressões do SET enxergam os valores **antigos**:
            // `SET a = b, b = a` troca os dois.
            let mut new = old.clone();
            for (col, expr) in &self.assignments {
                new[*col] = expr.eval(old)?;
            }
            update_row(pager, &self.table, *rid, old, new)?;
        }
        Ok(Some(count_tuple(targets.len())))
    }
}

pub struct Delete {
    table: TableInfo,
    input: Box<dyn Executor>,
    done: bool,
}

impl Delete {
    pub fn new(table: TableInfo, input: Box<dyn Executor>) -> Delete {
        Delete {
            table,
            input,
            done: false,
        }
    }
}

impl Executor for Delete {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        if self.done {
            return Ok(None);
        }
        self.done = true;
        let targets = collect_targets(self.input.as_mut(), pager)?;
        for (rid, values) in &targets {
            delete_row(pager, &self.table, *rid, values)?;
        }
        Ok(Some(count_tuple(targets.len())))
    }
}
