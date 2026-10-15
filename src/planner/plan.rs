//! O plano de execução: uma árvore de operadores.
//!
//! O planner transforma a AST num [`Plan`]. Cada nó é um operador que
//! recebe linhas dos filhos, faz algo com elas e passa adiante.
//!
//! ```text
//!   SELECT nome FROM users WHERE idade > 18 ORDER BY nome LIMIT 10
//!
//!   Limit 10
//!     -> Projection: nome
//!       -> Sort: nome
//!         -> Filter: idade > 18
//!           -> SeqScan: users
//! ```
//!
//! As linhas "sobem" das folhas (scans) até a raiz. Repare que a ordem é
//! diferente da ordem em que se escreve o SQL. Ela é a ordem **lógica** de
//! avaliação: FROM → WHERE → GROUP BY → HAVING → ORDER BY → SELECT →
//! DISTINCT → LIMIT.
//!
//! Você pode ver o plano de qualquer consulta com `EXPLAIN`.

use std::fmt;
use std::ops::Bound;

use crate::execution::expr::BoundExpr;
use crate::sql::ast::{JoinKind, fmt_literal};
use crate::types::Value;

#[derive(Debug, Clone)]
pub enum Plan {
    /// Produz exatamente uma linha vazia. Serve de entrada para `SELECT`
    /// sem `FROM`, como em `SELECT 1 + 1`.
    SingleRow,
    /// Lê todas as linhas da tabela, página por página.
    SeqScan {
        table: String,
    },
    /// Usa a B+Tree para ler só as linhas cujo valor da coluna está no
    /// intervalo `[lower, upper]`.
    IndexScan {
        table: String,
        index: String,
        lower: Bound<Value>,
        upper: Bound<Value>,
    },
    /// Deixa passar só as linhas em que o predicado é verdadeiro.
    Filter {
        input: Box<Plan>,
        predicate: BoundExpr,
    },
    /// Calcula as expressões do SELECT para cada linha.
    Projection {
        input: Box<Plan>,
        exprs: Vec<BoundExpr>,
    },
    /// Para cada linha da esquerda, percorre todas as da direita.
    NestedLoopJoin {
        left: Box<Plan>,
        right: Box<Plan>,
        kind: JoinKind,
        on: BoundExpr,
        /// Número de colunas da direita (para preencher com NULL no LEFT JOIN).
        right_width: usize,
    },
    /// Agrupa as linhas e calcula agregações. A saída tem as colunas do
    /// GROUP BY seguidas dos resultados das agregações.
    Aggregate {
        input: Box<Plan>,
        group_by: Vec<BoundExpr>,
        aggregates: Vec<AggCall>,
    },
    Sort {
        input: Box<Plan>,
        keys: Vec<SortKey>,
    },
    Distinct {
        input: Box<Plan>,
    },
    Limit {
        input: Box<Plan>,
        limit: Option<u64>,
        offset: u64,
    },
    /// Insere linhas já calculadas (os VALUES).
    Insert {
        table: String,
        rows: Vec<Vec<Value>>,
    },
    /// Altera as linhas produzidas por `input`.
    Update {
        table: String,
        input: Box<Plan>,
        /// (posição da coluna, novo valor)
        assignments: Vec<(usize, BoundExpr)>,
    },
    /// Remove as linhas produzidas por `input`.
    Delete {
        table: String,
        input: Box<Plan>,
    },
}

#[derive(Debug, Clone)]
pub struct SortKey {
    pub expr: BoundExpr,
    pub descending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl AggFunc {
    pub fn from_name(name: &str) -> Option<AggFunc> {
        Some(match name {
            "COUNT" => AggFunc::Count,
            "SUM" => AggFunc::Sum,
            "AVG" => AggFunc::Avg,
            "MIN" => AggFunc::Min,
            "MAX" => AggFunc::Max,
            _ => return None,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            AggFunc::Count => "COUNT",
            AggFunc::Sum => "SUM",
            AggFunc::Avg => "AVG",
            AggFunc::Min => "MIN",
            AggFunc::Max => "MAX",
        }
    }
}

/// Uma chamada de agregação, como `SUM(preco)`. `arg` é `None` só em
/// `COUNT(*)`.
#[derive(Debug, Clone, PartialEq)]
pub struct AggCall {
    pub func: AggFunc,
    pub arg: Option<BoundExpr>,
}

impl fmt::Display for AggCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.arg {
            Some(arg) => write!(f, "{}({arg})", self.func.name()),
            None => write!(f, "{}(*)", self.func.name()),
        }
    }
}

// ----------------------------------------------------------------------
// Display: o texto do EXPLAIN
// ----------------------------------------------------------------------

impl Plan {
    fn children(&self) -> Vec<&Plan> {
        match self {
            Plan::SingleRow | Plan::SeqScan { .. } | Plan::IndexScan { .. } | Plan::Insert { .. } => vec![],
            Plan::NestedLoopJoin { left, right, .. } => vec![left, right],
            Plan::Filter { input, .. }
            | Plan::Projection { input, .. }
            | Plan::Aggregate { input, .. }
            | Plan::Sort { input, .. }
            | Plan::Distinct { input }
            | Plan::Limit { input, .. }
            | Plan::Update { input, .. }
            | Plan::Delete { input, .. } => vec![input],
        }
    }

    /// Descrição de uma linha só deste nó (sem os filhos).
    fn describe(&self) -> String {
        let list = |exprs: &[BoundExpr]| exprs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(", ");
        match self {
            Plan::SingleRow => "SingleRow".into(),
            Plan::SeqScan { table } => format!("SeqScan: {table}"),
            Plan::IndexScan {
                table,
                index,
                lower,
                upper,
            } => {
                format!("IndexScan: {table} usando {index} {}", fmt_range(lower, upper))
            }
            Plan::Filter { predicate, .. } => format!("Filter: {predicate}"),
            Plan::Projection { exprs, .. } => format!("Projection: {}", list(exprs)),
            Plan::NestedLoopJoin { kind, on, .. } => {
                let kind = match kind {
                    JoinKind::Inner => "INNER",
                    JoinKind::Left => "LEFT",
                };
                format!("NestedLoopJoin ({kind}): {on}")
            }
            Plan::Aggregate {
                group_by, aggregates, ..
            } => {
                let aggs: Vec<String> = aggregates.iter().map(|a| a.to_string()).collect();
                if group_by.is_empty() {
                    format!("Aggregate: {}", aggs.join(", "))
                } else {
                    format!("Aggregate: {} agrupando por {}", aggs.join(", "), list(group_by))
                }
            }
            Plan::Sort { keys, .. } => {
                let keys: Vec<String> = keys
                    .iter()
                    .map(|k| format!("{}{}", k.expr, if k.descending { " DESC" } else { "" }))
                    .collect();
                format!("Sort: {}", keys.join(", "))
            }
            Plan::Distinct { .. } => "Distinct".into(),
            Plan::Limit { limit, offset, .. } => match limit {
                Some(n) => format!("Limit: {n} offset {offset}"),
                None => format!("Offset: {offset}"),
            },
            Plan::Insert { table, rows } => format!("Insert: {table} ({} linhas)", rows.len()),
            Plan::Update {
                table, assignments, ..
            } => {
                let sets: Vec<String> = assignments.iter().map(|(i, e)| format!("#{i} = {e}")).collect();
                format!("Update: {table} SET {}", sets.join(", "))
            }
            Plan::Delete { table, .. } => format!("Delete: {table}"),
        }
    }

    fn fmt_tree(&self, f: &mut fmt::Formatter<'_>, depth: usize) -> fmt::Result {
        if depth == 0 {
            writeln!(f, "{}", self.describe())?;
        } else {
            writeln!(f, "{}-> {}", "  ".repeat(depth), self.describe())?;
        }
        for child in self.children() {
            child.fmt_tree(f, depth + 1)?;
        }
        Ok(())
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_tree(f, 0)
    }
}

/// Mostra um intervalo como `[5, 10)`, `= 7`, `> 3`...
fn fmt_range(lower: &Bound<Value>, upper: &Bound<Value>) -> String {
    struct Lit<'a>(&'a Value);
    impl fmt::Display for Lit<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt_literal(self.0, f)
        }
    }
    match (lower, upper) {
        (Bound::Included(a), Bound::Included(b)) if a == b => format!("(= {})", Lit(a)),
        _ => {
            let lo = match lower {
                Bound::Included(v) => format!(">= {}", Lit(v)),
                Bound::Excluded(v) => format!("> {}", Lit(v)),
                Bound::Unbounded => String::new(),
            };
            let hi = match upper {
                Bound::Included(v) => format!("<= {}", Lit(v)),
                Bound::Excluded(v) => format!("< {}", Lit(v)),
                Bound::Unbounded => String::new(),
            };
            let parts: Vec<String> = [lo, hi].into_iter().filter(|s| !s.is_empty()).collect();
            format!("({})", parts.join(" e "))
        }
    }
}
