//! # Execução: rodando o plano
//!
//! Usamos o **modelo Volcano** (ou modelo de iteradores), o mesmo de
//! Postgres, MySQL e SQLite. Cada nó do plano vira um *executor* com um
//! único método:
//!
//! ```text
//!   next() -> próxima linha, ou None quando acabar
//! ```
//!
//! A raiz pede uma linha ao filho, que pede ao filho dele, e assim até o
//! scan, que lê do disco. As linhas sobem **uma de cada vez**, sem precisar
//! materializar resultados intermediários inteiros. Por isso um
//! `SELECT * FROM grande LIMIT 1` lê só uma linha.
//!
//! ```text
//!   Limit.next()
//!     └─ Projection.next()
//!          └─ Filter.next()       ← chama o filho até achar uma linha que passa
//!               └─ SeqScan.next() ← lê a próxima linha do heap
//! ```
//!
//! Alguns operadores são **bloqueantes**: Sort e Aggregate precisam ver
//! *todas* as linhas antes de devolver a primeira.
//!
//! Os executores não guardam referência ao `Pager`. Ele é passado a cada
//! `next()`. Assim cada executor é dono só do próprio estado, e o borrow
//! checker fica satisfeito sem `Rc<RefCell<...>>`.
//!
//! Arquivos:
//! - [`expr`] — avaliação de expressões.
//! - [`scan`] — SingleRow, SeqScan e IndexScan (as folhas do plano).
//! - [`transform`] — Filter, Projection, Distinct e Limit.
//! - [`join`] — NestedLoopJoin.
//! - [`sort`] e [`aggregate`] — os operadores bloqueantes.
//! - [`modify`] — INSERT, UPDATE e DELETE, com manutenção dos índices.

pub mod aggregate;
pub mod expr;
pub mod join;
pub mod modify;
pub mod scan;
pub mod sort;
pub mod transform;

use crate::catalog::Catalog;
use crate::error::Result;
use crate::heap::RecordId;
use crate::planner::plan::Plan;
use crate::storage::pager::Pager;
use crate::types::Value;

/// Uma linha em trânsito entre executores.
#[derive(Debug, Clone)]
pub struct Tuple {
    /// De onde a linha veio no heap. Só os scans preenchem. UPDATE e DELETE
    /// usam isso para saber qual linha alterar.
    pub rid: Option<RecordId>,
    pub values: Vec<Value>,
}

impl Tuple {
    pub fn new(values: Vec<Value>) -> Tuple {
        Tuple { rid: None, values }
    }
}

/// O contrato de todo operador do modelo Volcano.
pub trait Executor {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>>;
}

/// Transforma o plano numa árvore de executores. O catálogo é consultado
/// aqui, uma vez só. Os executores guardam o que precisam (heap, índices).
pub fn build(plan: &Plan, catalog: &Catalog) -> Result<Box<dyn Executor>> {
    let child = |p: &Plan| build(p, catalog);
    Ok(match plan {
        Plan::SingleRow => Box::new(scan::SingleRow::new()),
        Plan::SeqScan { table } => Box::new(scan::SeqScan::new(catalog.table(table)?)),
        Plan::IndexScan {
            table,
            index,
            lower,
            upper,
        } => Box::new(scan::IndexScan::new(
            catalog.table(table)?,
            index,
            lower.clone(),
            upper.clone(),
        )?),
        Plan::Filter { input, predicate } => {
            Box::new(transform::Filter::new(child(input)?, predicate.clone()))
        }
        Plan::Projection { input, exprs } => {
            Box::new(transform::Projection::new(child(input)?, exprs.clone()))
        }
        Plan::Distinct { input } => Box::new(transform::Distinct::new(child(input)?)),
        Plan::Limit { input, limit, offset } => {
            Box::new(transform::Limit::new(child(input)?, *limit, *offset))
        }
        Plan::NestedLoopJoin {
            left,
            right,
            kind,
            on,
            right_width,
        } => Box::new(join::NestedLoopJoin::new(
            child(left)?,
            child(right)?,
            *kind,
            on.clone(),
            *right_width,
        )),
        Plan::Sort { input, keys } => Box::new(sort::Sort::new(child(input)?, keys.clone())),
        Plan::Aggregate {
            input,
            group_by,
            aggregates,
        } => Box::new(aggregate::Aggregate::new(
            child(input)?,
            group_by.clone(),
            aggregates.clone(),
        )),
        Plan::Insert { table, rows } => {
            Box::new(modify::Insert::new(catalog.table(table)?.clone(), rows.clone()))
        }
        Plan::Update {
            table,
            input,
            assignments,
        } => Box::new(modify::Update::new(
            catalog.table(table)?.clone(),
            child(input)?,
            assignments.clone(),
        )),
        Plan::Delete { table, input } => {
            Box::new(modify::Delete::new(catalog.table(table)?.clone(), child(input)?))
        }
    })
}

/// Roda o plano até o fim e devolve todas as linhas.
pub fn execute(plan: &Plan, catalog: &Catalog, pager: &mut Pager) -> Result<Vec<Vec<Value>>> {
    let mut root = build(plan, catalog)?;
    let mut rows = Vec::new();
    while let Some(tuple) = root.next(pager)? {
        rows.push(tuple.values);
    }
    Ok(rows)
}
