//! O planner: da AST para o plano de execução.
//!
//! Ele faz três trabalhos:
//!
//! 1. **Validação**: as tabelas e colunas existem? `idade` é ambígua num
//!    JOIN? Uma coluna fora do GROUP BY aparece sem agregação?
//! 2. **Binding**: troca nomes de colunas pela posição delas na linha (veja
//!    `execution/expr.rs`).
//! 3. **Otimização** (bem simples): decide se dá para usar um índice em vez
//!    de ler a tabela inteira.
//!
//! # O otimizador
//!
//! Otimizadores reais estimam o custo de muitos planos alternativos (ordem
//! dos JOINs, algoritmo de cada JOIN, quais índices) usando estatísticas
//! das tabelas, e escolhem o mais barato. O nosso usa uma única regra:
//!
//! > Se o WHERE tem uma condição `coluna <op> constante` (ligada por AND ao
//! > resto), e a coluna tem índice, troque o SeqScan por um IndexScan.
//!
//! Igualdade (`=`) tem preferência sobre intervalos (`<`, `>=`...), porque
//! costuma selecionar menos linhas. O Filter com o WHERE completo continua
//! acima do IndexScan, então a correção não depende do índice: ele só
//! reduz quantas linhas o Filter precisa olhar.

use std::ops::Bound;

use crate::catalog::{CATALOG_TABLE, Catalog, TableInfo};
use crate::error::{DbError, Result};
use crate::execution::expr::{BoundExpr, ScalarFunction};
use crate::planner::plan::{AggCall, AggFunc, Plan, SortKey};
use crate::sql::ast::*;
use crate::types::{DataType, Value};

/// O plano mais os nomes das colunas do resultado.
#[derive(Debug)]
pub struct QueryPlan {
    pub plan: Plan,
    pub columns: Vec<String>,
}

/// Planeja um SELECT, INSERT, UPDATE ou DELETE. (Comandos de DDL e de
/// transação não têm plano: o `Database` os executa direto.)
pub fn plan_statement(stmt: &Statement, catalog: &Catalog) -> Result<QueryPlan> {
    let planner = Planner { catalog };
    match stmt {
        Statement::Select(select) => planner.plan_select(select),
        Statement::Insert(insert) => planner.plan_insert(insert),
        Statement::Update(update) => planner.plan_update(update),
        Statement::Delete(delete) => planner.plan_delete(delete),
        _ => Err(DbError::Plan(
            "só SELECT, INSERT, UPDATE e DELETE têm plano".into(),
        )),
    }
}

struct Planner<'a> {
    catalog: &'a Catalog,
}

// ----------------------------------------------------------------------
// Escopo: quais colunas estão visíveis e em que posição
// ----------------------------------------------------------------------

/// As colunas disponíveis num ponto da consulta. Num `FROM a JOIN b`, a
/// linha tem as colunas de `a` seguidas das de `b`, e o escopo registra
/// essa ordem.
#[derive(Debug, Clone, Default)]
struct Scope {
    columns: Vec<ScopeColumn>,
    /// Com mais de uma tabela, os nomes no EXPLAIN aparecem qualificados
    /// (`u.nome`).
    qualified: bool,
}

#[derive(Debug, Clone)]
struct ScopeColumn {
    /// Nome visível da tabela (o alias, se houver).
    table: String,
    name: String,
}

impl Scope {
    fn for_table(table: &TableInfo, visible_name: &str) -> Scope {
        Scope {
            columns: table
                .schema
                .columns
                .iter()
                .map(|c| ScopeColumn {
                    table: visible_name.to_string(),
                    name: c.name.clone(),
                })
                .collect(),
            qualified: false,
        }
    }

    fn join(mut self, right: Scope) -> Scope {
        self.columns.extend(right.columns);
        self.qualified = true;
        self
    }

    /// Acha a coluna `[tabela.]nome` e devolve a expressão ligada.
    fn resolve(&self, table: Option<&str>, name: &str) -> Result<BoundExpr> {
        let matches: Vec<usize> = self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name == name && table.is_none_or(|t| c.table == t))
            .map(|(i, _)| i)
            .collect();
        let full_name = match table {
            Some(t) => format!("{t}.{name}"),
            None => name.to_string(),
        };
        match matches.as_slice() {
            [] => Err(DbError::Plan(format!("a coluna '{full_name}' não existe"))),
            [index] => Ok(BoundExpr::Column {
                index: *index,
                name: self.display_name(*index),
            }),
            _ => Err(DbError::Plan(format!(
                "a coluna '{name}' é ambígua: existe em mais de uma tabela (use tabela.{name})"
            ))),
        }
    }

    fn display_name(&self, index: usize) -> String {
        let c = &self.columns[index];
        if self.qualified {
            format!("{}.{}", c.table, c.name)
        } else {
            c.name.clone()
        }
    }
}

// ----------------------------------------------------------------------
// Binding de expressões
// ----------------------------------------------------------------------

/// Gancho chamado em cada nó da expressão antes do binding normal. Se
/// devolver `Some`, o nó inteiro é substituído pelo valor devolvido. O
/// binder de agregação usa isso para trocar `SUM(x)` por uma referência à
/// saída do operador Aggregate.
type Hook<'h> = &'h mut dyn FnMut(&Expr) -> Result<Option<BoundExpr>>;

/// Binding sem agregações (WHERE, ON, SELECT simples...).
fn bind_expr(expr: &Expr, scope: &Scope) -> Result<BoundExpr> {
    bind_with(expr, scope, &mut |_| Ok(None))
}

fn bind_with(expr: &Expr, scope: &Scope, hook: Hook<'_>) -> Result<BoundExpr> {
    if let Some(replaced) = hook(expr)? {
        return Ok(replaced);
    }
    let mut bind = |e: &Expr| bind_with(e, scope, &mut *hook).map(Box::new);
    Ok(match expr {
        Expr::Literal(v) => BoundExpr::Literal(v.clone()),
        Expr::Column { table, name } => scope.resolve(table.as_deref(), name)?,
        Expr::Unary { op, expr } => BoundExpr::Unary {
            op: *op,
            expr: bind(expr)?,
        },
        Expr::Binary { op, left, right } => BoundExpr::Binary {
            op: *op,
            left: bind(left)?,
            right: bind(right)?,
        },
        Expr::IsNull { expr, negated } => BoundExpr::IsNull {
            expr: bind(expr)?,
            negated: *negated,
        },
        Expr::InList { expr, list, negated } => BoundExpr::InList {
            expr: bind(expr)?,
            list: list.iter().map(|e| bind(e).map(|b| *b)).collect::<Result<_>>()?,
            negated: *negated,
        },
        Expr::Function { name, args } => {
            if AggFunc::from_name(name).is_some() {
                return Err(DbError::Plan(format!(
                    "a agregação {name}() não pode ser usada aqui (no WHERE, use HAVING)"
                )));
            }
            let func = ScalarFunction::from_name(name)
                .ok_or_else(|| DbError::Plan(format!("função desconhecida: {name}()")))?;
            let (min, max) = func.arity();
            if args.len() < min || args.len() > max {
                return Err(DbError::Plan(format!(
                    "{name}() recebeu {} argumentos",
                    args.len()
                )));
            }
            BoundExpr::Function {
                func,
                args: args.iter().map(|e| bind(e).map(|b| *b)).collect::<Result<_>>()?,
            }
        }
    })
}

/// A expressão contém alguma função de agregação?
fn contains_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::Function { name, args } => {
            AggFunc::from_name(name).is_some() || args.iter().any(contains_aggregate)
        }
        Expr::Literal(_) | Expr::Column { .. } => false,
        Expr::Unary { expr, .. } | Expr::IsNull { expr, .. } => contains_aggregate(expr),
        Expr::Binary { left, right, .. } => contains_aggregate(left) || contains_aggregate(right),
        Expr::InList { expr, list, .. } => contains_aggregate(expr) || list.iter().any(contains_aggregate),
    }
}

/// Binding de expressões **acima** do operador Aggregate.
///
/// Depois do Aggregate, a linha tem esta forma:
///
/// ```text
///   [ grupo 0, grupo 1, ..., agregação 0, agregação 1, ... ]
/// ```
///
/// Então, em `SELECT cidade, COUNT(*) * 2 FROM t GROUP BY cidade`:
/// - `cidade` é igual ao grupo 0 e vira `#0`;
/// - `COUNT(*)` é registrada como agregação 0 e vira `#1`;
/// - `COUNT(*) * 2` vira `#1 * 2`.
///
/// Uma coluna que não está no GROUP BY nem dentro de agregação é erro: não
/// há um valor único dela por grupo.
struct AggBinder<'a> {
    scope: &'a Scope,
    groups: Vec<BoundExpr>,
    aggregates: Vec<AggCall>,
}

impl AggBinder<'_> {
    fn bind(&mut self, expr: &Expr) -> Result<BoundExpr> {
        let scope = self.scope;
        bind_with(expr, scope, &mut |e| self.replace(e))
    }

    fn replace(&mut self, expr: &Expr) -> Result<Option<BoundExpr>> {
        // A expressão é uma das do GROUP BY?
        if let Ok(bound) = bind_expr(expr, self.scope)
            && let Some(i) = self.groups.iter().position(|g| *g == bound)
        {
            return Ok(Some(BoundExpr::Column {
                index: i,
                name: bound.to_string(),
            }));
        }
        match expr {
            Expr::Function { name, args } if AggFunc::from_name(name).is_some() => {
                let func = AggFunc::from_name(name).unwrap();
                let arg = match args.as_slice() {
                    [] => None, // COUNT(*)
                    [arg] => Some(bind_expr(arg, self.scope)?),
                    _ => return Err(DbError::Plan(format!("{name}() recebe um único argumento"))),
                };
                let call = AggCall { func, arg };
                // A mesma agregação usada duas vezes é calculada uma vez só.
                let k = match self.aggregates.iter().position(|a| *a == call) {
                    Some(k) => k,
                    None => {
                        self.aggregates.push(call.clone());
                        self.aggregates.len() - 1
                    }
                };
                Ok(Some(BoundExpr::Column {
                    index: self.groups.len() + k,
                    name: call.to_string(),
                }))
            }
            Expr::Column { .. } => Err(DbError::Plan(format!(
                "a coluna '{expr}' precisa estar no GROUP BY ou dentro de uma agregação"
            ))),
            _ => Ok(None), // continua o binding normal nos filhos
        }
    }
}

// ----------------------------------------------------------------------
// SELECT
// ----------------------------------------------------------------------

impl Planner<'_> {
    fn plan_select(&self, s: &Select) -> Result<QueryPlan> {
        // FROM (+ JOINs) e WHERE
        let (mut plan, scope) = match &s.from {
            None => (Plan::SingleRow, Scope::default()),
            Some(from) if from.joins.is_empty() => {
                let table = self.catalog.table(&from.table.name)?;
                let scope = Scope::for_table(table, from.table.visible_name());
                let plan = self.scan_with_filter(table, &scope, s.where_clause.as_ref())?;
                (plan, scope)
            }
            Some(from) => {
                let (plan, scope) = self.plan_joins(from)?;
                let plan = match &s.where_clause {
                    Some(w) => Plan::Filter {
                        input: Box::new(plan),
                        predicate: bind_expr(w, &scope)?,
                    },
                    None => plan,
                };
                (plan, scope)
            }
        };
        if s.from.is_none() && s.where_clause.is_some() {
            return Err(DbError::Plan("WHERE exige FROM".into()));
        }

        // ORDER BY pode citar um alias ou a posição de um item do SELECT.
        let order_by: Vec<(Expr, bool)> = s
            .order_by
            .iter()
            .map(|o| Ok((resolve_order_expr(&o.expr, &s.projection)?, o.descending)))
            .collect::<Result<_>>()?;

        let select_exprs = || {
            s.projection.iter().filter_map(|item| match item {
                SelectItem::Expr { expr, .. } => Some(expr),
                SelectItem::Wildcard => None,
            })
        };
        let is_aggregate = !s.group_by.is_empty()
            || s.having.is_some()
            || select_exprs().any(contains_aggregate)
            || order_by.iter().any(|(e, _)| contains_aggregate(e));

        let (exprs, sort_keys) = if is_aggregate {
            let mut binder = AggBinder {
                scope: &scope,
                groups: s
                    .group_by
                    .iter()
                    .map(|g| bind_expr(g, &scope))
                    .collect::<Result<_>>()?,
                aggregates: Vec::new(),
            };
            let exprs = s
                .projection
                .iter()
                .map(|item| match item {
                    SelectItem::Expr { expr, .. } => binder.bind(expr),
                    SelectItem::Wildcard => {
                        Err(DbError::Plan("SELECT * não pode ser usado com agregação".into()))
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            let having = s.having.as_ref().map(|h| binder.bind(h)).transpose()?;
            let sort_keys = order_by
                .iter()
                .map(|(e, desc)| {
                    Ok(SortKey {
                        expr: binder.bind(e)?,
                        descending: *desc,
                    })
                })
                .collect::<Result<Vec<_>>>()?;

            plan = Plan::Aggregate {
                input: Box::new(plan),
                group_by: binder.groups,
                aggregates: binder.aggregates,
            };
            if let Some(predicate) = having {
                plan = Plan::Filter {
                    input: Box::new(plan),
                    predicate,
                };
            }
            (exprs, sort_keys)
        } else {
            let mut exprs = Vec::new();
            for item in &s.projection {
                match item {
                    SelectItem::Expr { expr, .. } => exprs.push(bind_expr(expr, &scope)?),
                    SelectItem::Wildcard => {
                        if scope.columns.is_empty() {
                            return Err(DbError::Plan("SELECT * exige FROM".into()));
                        }
                        for i in 0..scope.columns.len() {
                            exprs.push(BoundExpr::Column {
                                index: i,
                                name: scope.display_name(i),
                            });
                        }
                    }
                }
            }
            let sort_keys = order_by
                .iter()
                .map(|(e, desc)| {
                    Ok(SortKey {
                        expr: bind_expr(e, &scope)?,
                        descending: *desc,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            (exprs, sort_keys)
        };

        // Sort fica ANTES da Projection, para poder ordenar por colunas
        // que não aparecem no resultado.
        if !sort_keys.is_empty() {
            plan = Plan::Sort {
                input: Box::new(plan),
                keys: sort_keys,
            };
        }
        plan = Plan::Projection {
            input: Box::new(plan),
            exprs,
        };
        if s.distinct {
            plan = Plan::Distinct {
                input: Box::new(plan),
            };
        }
        if s.limit.is_some() || s.offset.is_some() {
            plan = Plan::Limit {
                input: Box::new(plan),
                limit: s.limit,
                offset: s.offset.unwrap_or(0),
            };
        }

        Ok(QueryPlan {
            plan,
            columns: output_names(&s.projection, &scope),
        })
    }

    /// FROM com JOINs: cada JOIN vira um NestedLoopJoin cuja esquerda é tudo
    /// o que veio antes.
    fn plan_joins(&self, from: &FromClause) -> Result<(Plan, Scope)> {
        let mut seen = vec![from.table.visible_name()];
        let first = self.catalog.table(&from.table.name)?;
        let mut scope = Scope::for_table(first, from.table.visible_name());
        let mut plan = Plan::SeqScan {
            table: first.name().to_string(),
        };
        for join in &from.joins {
            let visible = join.table.visible_name();
            if seen.contains(&visible) {
                return Err(DbError::Plan(format!(
                    "a tabela '{visible}' aparece duas vezes no FROM; use um alias"
                )));
            }
            seen.push(visible);
            let table = self.catalog.table(&join.table.name)?;
            let right_scope = Scope::for_table(table, visible);
            let right_width = right_scope.columns.len();
            scope = scope.join(right_scope);
            plan = Plan::NestedLoopJoin {
                left: Box::new(plan),
                right: Box::new(Plan::SeqScan {
                    table: table.name().to_string(),
                }),
                kind: join.kind,
                on: bind_expr(&join.on, &scope)?,
                right_width,
            };
        }
        Ok((plan, scope))
    }

    /// Scan de uma tabela + Filter do WHERE, escolhendo índice se possível.
    fn scan_with_filter(
        &self,
        table: &TableInfo,
        scope: &Scope,
        where_clause: Option<&Expr>,
    ) -> Result<Plan> {
        let Some(where_clause) = where_clause else {
            return Ok(Plan::SeqScan {
                table: table.name().to_string(),
            });
        };
        let predicate = bind_expr(where_clause, scope)?;
        Ok(Plan::Filter {
            input: Box::new(choose_access_path(table, &predicate)),
            predicate,
        })
    }

    // ------------------------------------------------------------------
    // INSERT, UPDATE, DELETE
    // ------------------------------------------------------------------

    fn writable_table(&self, name: &str) -> Result<&TableInfo> {
        if name == CATALOG_TABLE {
            return Err(DbError::Plan(format!(
                "'{CATALOG_TABLE}' é mantida pelo banco; use CREATE/DROP"
            )));
        }
        self.catalog.table(name)
    }

    fn plan_insert(&self, ins: &Insert) -> Result<QueryPlan> {
        let table = self.writable_table(&ins.table)?;
        let width = table.schema.columns.len();

        // Para cada valor informado, em qual coluna ele vai.
        let targets: Vec<usize> = match &ins.columns {
            None => (0..width).collect(),
            Some(cols) => {
                let mut targets = Vec::new();
                for col in cols {
                    let idx = table.schema.column_index(col).ok_or_else(|| {
                        DbError::Plan(format!("a coluna '{col}' não existe em '{}'", ins.table))
                    })?;
                    if targets.contains(&idx) {
                        return Err(DbError::Plan(format!("coluna '{col}' repetida no INSERT")));
                    }
                    targets.push(idx);
                }
                targets
            }
        };

        let mut rows = Vec::new();
        for (n, exprs) in ins.rows.iter().enumerate() {
            if exprs.len() != targets.len() {
                return Err(DbError::Plan(format!(
                    "a linha {} tem {} valores, mas eram esperados {}",
                    n + 1,
                    exprs.len(),
                    targets.len()
                )));
            }
            // Colunas omitidas ficam NULL.
            let mut values = vec![Value::Null; width];
            for (expr, &col) in exprs.iter().zip(&targets) {
                // VALUES não pode citar colunas: o escopo é vazio, e a
                // expressão é calculada já aqui, no planejamento.
                values[col] = bind_expr(expr, &Scope::default())?.eval(&[])?;
            }
            rows.push(values);
        }
        Ok(QueryPlan {
            plan: Plan::Insert {
                table: table.name().to_string(),
                rows,
            },
            columns: vec![],
        })
    }

    fn plan_update(&self, upd: &Update) -> Result<QueryPlan> {
        let table = self.writable_table(&upd.table)?;
        let scope = Scope::for_table(table, &upd.table);
        let assignments = upd
            .assignments
            .iter()
            .map(|(col, expr)| {
                let idx = table.schema.column_index(col).ok_or_else(|| {
                    DbError::Plan(format!("a coluna '{col}' não existe em '{}'", upd.table))
                })?;
                Ok((idx, bind_expr(expr, &scope)?))
            })
            .collect::<Result<_>>()?;
        Ok(QueryPlan {
            plan: Plan::Update {
                table: table.name().to_string(),
                input: Box::new(self.scan_with_filter(table, &scope, upd.where_clause.as_ref())?),
                assignments,
            },
            columns: vec![],
        })
    }

    fn plan_delete(&self, del: &Delete) -> Result<QueryPlan> {
        let table = self.writable_table(&del.table)?;
        let scope = Scope::for_table(table, &del.table);
        Ok(QueryPlan {
            plan: Plan::Delete {
                table: table.name().to_string(),
                input: Box::new(self.scan_with_filter(table, &scope, del.where_clause.as_ref())?),
            },
            columns: vec![],
        })
    }
}

// ----------------------------------------------------------------------
// Escolha do caminho de acesso (o "otimizador")
// ----------------------------------------------------------------------

/// Decide entre SeqScan e IndexScan para uma tabela, dado o WHERE.
fn choose_access_path(table: &TableInfo, predicate: &BoundExpr) -> Plan {
    let mut best: Option<(bool, Plan)> = None; // (é igualdade?, plano)

    for conjunct in conjuncts(predicate) {
        let Some((col, op, value)) = column_vs_constant(conjunct) else {
            continue;
        };
        let Some(index) = table.index_on(col) else {
            continue;
        };
        if !comparable(table.schema.columns[col].data_type, &value) {
            continue;
        }
        let (lower, upper) = match op {
            BinaryOp::Eq => (Bound::Included(value.clone()), Bound::Included(value)),
            BinaryOp::Gt => (Bound::Excluded(value), Bound::Unbounded),
            BinaryOp::GtEq => (Bound::Included(value), Bound::Unbounded),
            BinaryOp::Lt => (Bound::Unbounded, Bound::Excluded(value)),
            BinaryOp::LtEq => (Bound::Unbounded, Bound::Included(value)),
            _ => continue,
        };
        let is_equality = op == BinaryOp::Eq;
        if best
            .as_ref()
            .is_none_or(|(best_is_eq, _)| is_equality && !best_is_eq)
        {
            let plan = Plan::IndexScan {
                table: table.name().to_string(),
                index: index.name.clone(),
                lower,
                upper,
            };
            best = Some((is_equality, plan));
        }
    }

    best.map(|(_, plan)| plan).unwrap_or_else(|| Plan::SeqScan {
        table: table.name().to_string(),
    })
}

/// Separa `a AND b AND c` em `[a, b, c]`.
fn conjuncts(expr: &BoundExpr) -> Vec<&BoundExpr> {
    match expr {
        BoundExpr::Binary {
            op: BinaryOp::And,
            left,
            right,
        } => {
            let mut v = conjuncts(left);
            v.extend(conjuncts(right));
            v
        }
        other => vec![other],
    }
}

/// Reconhece `coluna <op> constante` (ou `constante <op> coluna`, invertendo
/// o operador).
fn column_vs_constant(expr: &BoundExpr) -> Option<(usize, BinaryOp, Value)> {
    let BoundExpr::Binary { op, left, right } = expr else {
        return None;
    };
    match (left.as_ref(), right.as_ref()) {
        (BoundExpr::Column { index, .. }, BoundExpr::Literal(v)) if !v.is_null() => {
            Some((*index, *op, v.clone()))
        }
        (BoundExpr::Literal(v), BoundExpr::Column { index, .. }) if !v.is_null() => {
            let flipped = match op {
                BinaryOp::Lt => BinaryOp::Gt,
                BinaryOp::LtEq => BinaryOp::GtEq,
                BinaryOp::Gt => BinaryOp::Lt,
                BinaryOp::GtEq => BinaryOp::LtEq,
                other => *other,
            };
            Some((*index, flipped, v.clone()))
        }
        _ => None,
    }
}

/// O valor pode ser comparado com a coluna? (Se não puder, deixamos o Filter
/// reportar o erro de tipo em vez de o índice "esconder" o problema.)
fn comparable(column_type: DataType, value: &Value) -> bool {
    matches!(
        (column_type, value),
        (
            DataType::Integer | DataType::Float,
            Value::Integer(_) | Value::Float(_)
        ) | (DataType::Text, Value::Text(_))
            | (DataType::Boolean, Value::Boolean(_))
    )
}

// ----------------------------------------------------------------------
// Auxiliares do SELECT
// ----------------------------------------------------------------------

/// `ORDER BY 2` → o segundo item do SELECT; `ORDER BY total` → o item com
/// alias `total`; qualquer outra coisa fica como está.
fn resolve_order_expr(expr: &Expr, projection: &[SelectItem]) -> Result<Expr> {
    match expr {
        Expr::Literal(Value::Integer(n)) => {
            let item = usize::try_from(*n)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| projection.get(i))
                .ok_or_else(|| DbError::Plan(format!("ORDER BY {n}: não existe o item {n} no SELECT")))?;
            match item {
                SelectItem::Expr { expr, .. } => Ok(expr.clone()),
                SelectItem::Wildcard => Err(DbError::Plan("ORDER BY por posição não funciona com *".into())),
            }
        }
        Expr::Column { table: None, name } => {
            let aliased = projection.iter().find_map(|item| match item {
                SelectItem::Expr { expr, alias: Some(a) } if a == name => Some(expr.clone()),
                _ => None,
            });
            Ok(aliased.unwrap_or_else(|| expr.clone()))
        }
        other => Ok(other.clone()),
    }
}

/// Nome de cada coluna do resultado: o alias, o nome da coluna ou o texto
/// da expressão.
fn output_names(projection: &[SelectItem], scope: &Scope) -> Vec<String> {
    let mut names = Vec::new();
    for item in projection {
        match item {
            SelectItem::Wildcard => names.extend(scope.columns.iter().map(|c| c.name.clone())),
            SelectItem::Expr {
                alias: Some(alias), ..
            } => names.push(alias.clone()),
            SelectItem::Expr {
                expr: Expr::Column { name, .. },
                ..
            } => names.push(name.clone()),
            SelectItem::Expr { expr, .. } => names.push(expr.to_string()),
        }
    }
    names
}
