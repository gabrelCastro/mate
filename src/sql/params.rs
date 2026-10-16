//! Parâmetros: `?` no SQL, com os valores passados à parte.
//!
//! ```text
//!   db.query_with_params("SELECT * FROM users WHERE nome = ?", &[Value::Text(nome)])
//! ```
//!
//! # Por que não montar o texto do SQL com os valores?
//!
//! Porque o valor pode conter SQL. Se `nome` vier de um formulário com
//! `x' OR '1'='1`, montar `"... WHERE nome = '" + nome + "'"` gera
//! `WHERE nome = 'x' OR '1'='1'`, que devolve a tabela inteira. Esse é o
//! ataque de **SQL injection**.
//!
//! Com parâmetros, o valor nunca passa pelo lexer nem pelo parser. O SQL é
//! analisado com um buraco (`Expr::Param`), e o valor é encaixado depois,
//! já como um `Value` pronto. Não importa o que o texto contenha: ele é
//! sempre só um texto.
//!
//! # Como funciona aqui
//!
//! O parser numera os `?` na ordem em que aparecem (0, 1, 2...). Antes de
//! planejar, [`bind_params`] percorre a AST e troca cada `Expr::Param(n)`
//! por `Expr::Literal(valores[n])`. Daí em diante, o planner e os
//! executores enxergam literais comuns.
//!
//! Bancos reais vão além: guardam o plano já pronto (*prepared statement*)
//! e só trocam os valores a cada execução, economizando o parse e o
//! planejamento.

use crate::error::{DbError, Result};
use crate::sql::ast::{Expr, SelectItem, Statement};
use crate::types::Value;

/// Substitui os `?` do comando pelos valores, na ordem. O número de valores
/// precisa ser igual ao número de `?`.
pub fn bind_params(stmt: &mut Statement, params: &[Value]) -> Result<()> {
    let mut used = 0;
    let mut missing = false;
    for_each_expr(stmt, &mut |expr| {
        if let Expr::Param(n) = expr {
            used = used.max(*n + 1);
            match params.get(*n) {
                Some(value) => *expr = Expr::Literal(value.clone()),
                None => missing = true,
            }
        }
    });
    if missing || used != params.len() {
        return Err(DbError::Plan(format!(
            "o comando tem {used} parâmetro(s) '?', mas recebeu {} valor(es)",
            params.len()
        )));
    }
    Ok(())
}

/// Chama `f` em toda expressão do comando, inclusive nas subexpressões.
fn for_each_expr(stmt: &mut Statement, f: &mut dyn FnMut(&mut Expr)) {
    match stmt {
        Statement::Select(select) => {
            for item in &mut select.projection {
                if let SelectItem::Expr { expr, .. } = item {
                    walk(expr, f);
                }
            }
            if let Some(from) = &mut select.from {
                for join in &mut from.joins {
                    walk(&mut join.on, f);
                }
            }
            for expr in select
                .where_clause
                .iter_mut()
                .chain(&mut select.group_by)
                .chain(select.having.iter_mut())
                .chain(select.order_by.iter_mut().map(|o| &mut o.expr))
            {
                walk(expr, f);
            }
        }
        Statement::Insert(insert) => {
            for expr in insert.rows.iter_mut().flatten() {
                walk(expr, f);
            }
        }
        Statement::Update(update) => {
            for (_, expr) in &mut update.assignments {
                walk(expr, f);
            }
            if let Some(expr) = &mut update.where_clause {
                walk(expr, f);
            }
        }
        Statement::Delete(delete) => {
            if let Some(expr) = &mut delete.where_clause {
                walk(expr, f);
            }
        }
        Statement::Explain(inner) => for_each_expr(inner, f),
        Statement::CreateTable(_)
        | Statement::CreateIndex(_)
        | Statement::DropTable { .. }
        | Statement::Begin
        | Statement::Commit
        | Statement::Rollback => {}
    }
}

/// Visita a expressão e, depois, cada filho dela.
fn walk(expr: &mut Expr, f: &mut dyn FnMut(&mut Expr)) {
    f(expr);
    match expr {
        Expr::Literal(_) | Expr::Param(_) | Expr::Column { .. } => {}
        Expr::Unary { expr, .. } | Expr::IsNull { expr, .. } => walk(expr, f),
        Expr::Binary { left, right, .. } => {
            walk(left, f);
            walk(right, f);
        }
        Expr::InList { expr, list, .. } => {
            walk(expr, f);
            for item in list {
                walk(item, f);
            }
        }
        Expr::Function { args, .. } => {
            for arg in args {
                walk(arg, f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::parse_one;

    #[test]
    fn troca_os_parametros_na_ordem() {
        let mut stmt = parse_one("SELECT ? + 1 FROM t WHERE a = ? AND b IN (?, 3)").unwrap();
        bind_params(
            &mut stmt,
            &[Value::Integer(10), Value::Text("x".into()), Value::Null],
        )
        .unwrap();
        let Statement::Select(s) = &stmt else { panic!() };
        assert_eq!(
            s.where_clause.as_ref().unwrap().to_string(),
            "(a = 'x') AND b IN (NULL, 3)"
        );
    }

    #[test]
    fn numero_errado_de_valores() {
        let mut stmt = parse_one("SELECT * FROM t WHERE a = ?").unwrap();
        assert!(bind_params(&mut stmt.clone(), &[]).is_err());
        assert!(bind_params(&mut stmt, &[Value::Integer(1), Value::Integer(2)]).is_err());
    }

    #[test]
    fn texto_malicioso_continua_sendo_texto() {
        let mut stmt = parse_one("SELECT * FROM t WHERE nome = ?").unwrap();
        bind_params(&mut stmt, &[Value::Text("x' OR '1'='1".into())]).unwrap();
        let Statement::Select(s) = &stmt else { panic!() };
        // Uma única comparação com um texto esquisito, e não um OR.
        assert!(matches!(
            s.where_clause,
            Some(Expr::Binary { ref right, .. }) if **right == Expr::Literal(Value::Text("x' OR '1'='1".into()))
        ));
    }
}
