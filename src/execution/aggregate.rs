//! GROUP BY e funções de agregação (COUNT, SUM, AVG, MIN, MAX).
//!
//! Usamos **agregação por hash** (aqui, por árvore: um `BTreeMap`):
//!
//! ```text
//!   para cada linha:
//!       chave = valores do GROUP BY
//!       acumuladores = mapa[chave] (cria se não existir)
//!       atualiza cada acumulador com a linha
//!   para cada (chave, acumuladores) do mapa:
//!       devolve chave ++ resultados
//! ```
//!
//! Um **acumulador** guarda o estado parcial de uma agregação. Para AVG,
//! por exemplo, é a soma e a contagem, e o resultado é a divisão no final.
//!
//! Como usamos `BTreeMap`, os grupos saem ordenados pela chave. Um banco
//! com `HashMap` devolveria em ordem arbitrária, e é por isso que SQL não
//! garante ordem nenhuma sem ORDER BY.
//!
//! Regras do SQL que valem lembrar:
//! - agregações ignoram NULLs (`COUNT(coluna)` conta só não-nulos), menos
//!   o `COUNT(*)`, que conta linhas;
//! - sem GROUP BY, toda a tabela é um grupo só, e mesmo vazia ela produz
//!   uma linha (`SELECT COUNT(*) FROM vazia` devolve 0);
//! - SUM/AVG/MIN/MAX de nenhum valor é NULL.

use std::collections::BTreeMap;

use crate::error::{DbError, Result};
use crate::execution::expr::BoundExpr;
use crate::execution::{Executor, Tuple};
use crate::planner::plan::{AggCall, AggFunc};
use crate::storage::pager::Pager;
use crate::types::Value;

pub struct Aggregate {
    input: Box<dyn Executor>,
    group_by: Vec<BoundExpr>,
    aggregates: Vec<AggCall>,
    /// Resultado pronto, de trás para frente (para usar `pop`).
    output: Option<Vec<Tuple>>,
}

impl Aggregate {
    pub fn new(input: Box<dyn Executor>, group_by: Vec<BoundExpr>, aggregates: Vec<AggCall>) -> Aggregate {
        Aggregate {
            input,
            group_by,
            aggregates,
            output: None,
        }
    }

    fn compute(&mut self, pager: &mut Pager) -> Result<Vec<Tuple>> {
        let mut groups: BTreeMap<Vec<Value>, Vec<Accumulator>> = BTreeMap::new();

        while let Some(tuple) = self.input.next(pager)? {
            let key = self
                .group_by
                .iter()
                .map(|e| e.eval(&tuple.values))
                .collect::<Result<Vec<_>>>()?;
            let accs = groups
                .entry(key)
                .or_insert_with(|| self.aggregates.iter().map(|a| Accumulator::new(a.func)).collect());
            for (acc, call) in accs.iter_mut().zip(&self.aggregates) {
                match &call.arg {
                    None => acc.add_row(),                           // COUNT(*)
                    Some(arg) => acc.add(arg.eval(&tuple.values)?)?, // COUNT(x), SUM(x)...
                }
            }
        }

        // Sem GROUP BY, uma tabela vazia ainda produz um grupo.
        if groups.is_empty() && self.group_by.is_empty() {
            let accs = self.aggregates.iter().map(|a| Accumulator::new(a.func)).collect();
            groups.insert(vec![], accs);
        }

        Ok(groups
            .into_iter()
            .map(|(mut key, accs)| {
                key.extend(accs.into_iter().map(Accumulator::finish));
                Tuple::new(key)
            })
            .rev()
            .collect())
    }
}

impl Executor for Aggregate {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        if self.output.is_none() {
            self.output = Some(self.compute(pager)?);
        }
        Ok(self.output.as_mut().unwrap().pop())
    }
}

/// O estado parcial de uma agregação.
enum Accumulator {
    Count(i64),
    /// Soma até agora (`None` se ainda não viu valores).
    Sum(Option<Value>),
    Avg {
        sum: f64,
        count: i64,
    },
    Min(Option<Value>),
    Max(Option<Value>),
}

impl Accumulator {
    fn new(func: AggFunc) -> Accumulator {
        match func {
            AggFunc::Count => Accumulator::Count(0),
            AggFunc::Sum => Accumulator::Sum(None),
            AggFunc::Avg => Accumulator::Avg { sum: 0.0, count: 0 },
            AggFunc::Min => Accumulator::Min(None),
            AggFunc::Max => Accumulator::Max(None),
        }
    }

    /// COUNT(*): conta a linha, sem olhar valores.
    fn add_row(&mut self) {
        if let Accumulator::Count(n) = self {
            *n += 1;
        }
    }

    fn add(&mut self, value: Value) -> Result<()> {
        if value.is_null() {
            return Ok(()); // agregações ignoram NULL
        }
        match self {
            Accumulator::Count(n) => *n += 1,
            Accumulator::Sum(sum) => {
                *sum = Some(match (sum.take(), value) {
                    (None, v @ (Value::Integer(_) | Value::Float(_))) => v,
                    (Some(Value::Integer(a)), Value::Integer(b)) => Value::Integer(
                        a.checked_add(b)
                            .ok_or_else(|| DbError::Execution("estouro de inteiro no SUM".into()))?,
                    ),
                    (Some(a), b) => match (to_f64(&a), to_f64(&b)) {
                        (Some(a), Some(b)) => Value::Float(a + b),
                        _ => return Err(not_numeric("SUM", &b)),
                    },
                    (None, v) => return Err(not_numeric("SUM", &v)),
                });
            }
            Accumulator::Avg { sum, count } => {
                *sum += to_f64(&value).ok_or_else(|| not_numeric("AVG", &value))?;
                *count += 1;
            }
            Accumulator::Min(min) => {
                if min.as_ref().is_none_or(|m| value < *m) {
                    *min = Some(value);
                }
            }
            Accumulator::Max(max) => {
                if max.as_ref().is_none_or(|m| value > *m) {
                    *max = Some(value);
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Value {
        match self {
            Accumulator::Count(n) => Value::Integer(n),
            Accumulator::Sum(sum) => sum.unwrap_or(Value::Null),
            Accumulator::Avg { count: 0, .. } => Value::Null,
            Accumulator::Avg { sum, count } => Value::Float(sum / count as f64),
            Accumulator::Min(v) | Accumulator::Max(v) => v.unwrap_or(Value::Null),
        }
    }
}

fn to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Float(x) => Some(*x),
        _ => None,
    }
}

fn not_numeric(func: &str, v: &Value) -> DbError {
    DbError::Execution(format!("{func}() precisa de números, recebeu {v}"))
}
