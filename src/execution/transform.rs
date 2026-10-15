//! Executores que transformam o fluxo de linhas um a um (não bloqueantes).

use std::collections::BTreeSet;

use crate::error::Result;
use crate::execution::expr::BoundExpr;
use crate::execution::{Executor, Tuple};
use crate::storage::pager::Pager;
use crate::types::Value;

/// WHERE / HAVING: repassa só as linhas em que o predicado é verdadeiro.
pub struct Filter {
    input: Box<dyn Executor>,
    predicate: BoundExpr,
}

impl Filter {
    pub fn new(input: Box<dyn Executor>, predicate: BoundExpr) -> Filter {
        Filter { input, predicate }
    }
}

impl Executor for Filter {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        while let Some(tuple) = self.input.next(pager)? {
            if self.predicate.eval_condition(&tuple.values)? {
                return Ok(Some(tuple));
            }
        }
        Ok(None)
    }
}

/// A lista do SELECT: calcula as expressões de saída para cada linha.
pub struct Projection {
    input: Box<dyn Executor>,
    exprs: Vec<BoundExpr>,
}

impl Projection {
    pub fn new(input: Box<dyn Executor>, exprs: Vec<BoundExpr>) -> Projection {
        Projection { input, exprs }
    }
}

impl Executor for Projection {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        let Some(tuple) = self.input.next(pager)? else {
            return Ok(None);
        };
        let values = self
            .exprs
            .iter()
            .map(|e| e.eval(&tuple.values))
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Tuple::new(values)))
    }
}

/// SELECT DISTINCT: lembra as linhas já vistas e descarta repetições.
/// Não é bloqueante, mas usa memória proporcional ao número de linhas
/// distintas.
pub struct Distinct {
    input: Box<dyn Executor>,
    seen: BTreeSet<Vec<Value>>,
}

impl Distinct {
    pub fn new(input: Box<dyn Executor>) -> Distinct {
        Distinct {
            input,
            seen: BTreeSet::new(),
        }
    }
}

impl Executor for Distinct {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        while let Some(tuple) = self.input.next(pager)? {
            // `insert` devolve false se o valor já estava no conjunto.
            if self.seen.insert(tuple.values.clone()) {
                return Ok(Some(tuple));
            }
        }
        Ok(None)
    }
}

/// LIMIT / OFFSET: pula `offset` linhas e para depois de `limit`. Quando
/// atinge o limite, não pede mais linhas ao filho, e a consulta termina
/// cedo.
pub struct Limit {
    input: Box<dyn Executor>,
    limit: Option<u64>,
    offset: u64,
    returned: u64,
}

impl Limit {
    pub fn new(input: Box<dyn Executor>, limit: Option<u64>, offset: u64) -> Limit {
        Limit {
            input,
            limit,
            offset,
            returned: 0,
        }
    }
}

impl Executor for Limit {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        while self.offset > 0 {
            if self.input.next(pager)?.is_none() {
                return Ok(None);
            }
            self.offset -= 1;
        }
        if self.limit.is_some_and(|limit| self.returned >= limit) {
            return Ok(None);
        }
        let tuple = self.input.next(pager)?;
        if tuple.is_some() {
            self.returned += 1;
        }
        Ok(tuple)
    }
}
