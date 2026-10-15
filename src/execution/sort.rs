//! ORDER BY: um operador bloqueante.
//!
//! Para devolver a menor linha, o Sort precisa ter visto todas. Então, no
//! primeiro `next`, ele lê a entrada inteira, ordena em memória e depois vai
//! devolvendo.
//!
//! Se os dados não cabem na memória, bancos reais usam **ordenação
//! externa**: ordenam pedaços, gravam cada pedaço ordenado em disco e depois
//! intercalam (*merge*) os pedaços.
//!
//! NULLs são os menores valores na nossa ordem total (veja `Value::cmp`),
//! então aparecem primeiro em ASC e por último em DESC, como no SQLite e
//! no MySQL. O Postgres faz o contrário.

use std::cmp::Ordering;

use crate::error::Result;
use crate::execution::{Executor, Tuple};
use crate::planner::plan::SortKey;
use crate::storage::pager::Pager;
use crate::types::Value;

pub struct Sort {
    input: Box<dyn Executor>,
    keys: Vec<SortKey>,
    /// Linhas já ordenadas, de trás para frente (para usar `pop`).
    sorted: Option<Vec<Tuple>>,
}

impl Sort {
    pub fn new(input: Box<dyn Executor>, keys: Vec<SortKey>) -> Sort {
        Sort {
            input,
            keys,
            sorted: None,
        }
    }

    fn compare(&self, a: &[Value], b: &[Value]) -> Ordering {
        for (key, (va, vb)) in self.keys.iter().zip(a.iter().zip(b)) {
            let ord = va.cmp(vb);
            let ord = if key.descending { ord.reverse() } else { ord };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    }
}

impl Executor for Sort {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        if self.sorted.is_none() {
            // Calcula as chaves de ordenação uma vez por linha, antes de
            // ordenar (e não a cada comparação).
            let mut rows: Vec<(Vec<Value>, Tuple)> = Vec::new();
            while let Some(tuple) = self.input.next(pager)? {
                let key = self
                    .keys
                    .iter()
                    .map(|k| k.expr.eval(&tuple.values))
                    .collect::<Result<Vec<_>>>()?;
                rows.push((key, tuple));
            }
            // `sort_by` é estável: linhas empatadas mantêm a ordem original.
            rows.sort_by(|(ka, _), (kb, _)| self.compare(ka, kb));
            self.sorted = Some(rows.into_iter().rev().map(|(_, t)| t).collect());
        }
        Ok(self.sorted.as_mut().unwrap().pop())
    }
}
