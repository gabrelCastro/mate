//! Expressões "ligadas" (*bound*) e sua avaliação.
//!
//! Na AST, uma coluna é um **nome** (`users.idade`). Durante a execução, uma
//! linha é só um vetor de valores, e procurar nomes a cada linha seria
//! lento e repetitivo. Por isso o planner faz o *binding*: troca cada nome
//! pela **posição** da coluna na linha.
//!
//! ```text
//!   AST:        idade > 18           (Column "idade")
//!   Bound:      #2 > 18              (Column index 2)
//!   Avaliação:  row[2] > 18          → Value::Boolean(true)
//! ```
//!
//! O nome continua guardado em `BoundExpr::Column`, mas só para o EXPLAIN
//! ficar legível.
//!
//! # Lógica de três valores
//!
//! Em SQL, uma condição pode ser verdadeira, falsa ou **desconhecida**
//! (NULL). `NULL = 1` não é falso: é desconhecido. As tabelas de AND e OR
//! seguem isso:
//!
//! ```text
//!   AND   | T     F     NULL         OR    | T     F     NULL
//!   ------+------------------        ------+------------------
//!   T     | T     F     NULL         T     | T     T     T
//!   F     | F     F     F            F     | T     F     NULL
//!   NULL  | NULL  F     NULL         NULL  | T     NULL  NULL
//! ```
//!
//! O WHERE só deixa passar linhas cuja condição é **verdadeira**. Por isso
//! `WHERE x = NULL` nunca encontra nada e o certo é `WHERE x IS NULL`.

use std::cmp::Ordering;
use std::fmt;

use crate::error::{DbError, Result};
use crate::sql::ast::{BinaryOp, UnaryOp, fmt_literal};
use crate::types::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum BoundExpr {
    Literal(Value),
    Column {
        index: usize,
        name: String,
    },
    Unary {
        op: UnaryOp,
        expr: Box<BoundExpr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<BoundExpr>,
        right: Box<BoundExpr>,
    },
    IsNull {
        expr: Box<BoundExpr>,
        negated: bool,
    },
    InList {
        expr: Box<BoundExpr>,
        list: Vec<BoundExpr>,
        negated: bool,
    },
    Function {
        func: ScalarFunction,
        args: Vec<BoundExpr>,
    },
}

/// Funções escalares: recebem valores de uma linha e devolvem um valor.
/// (As de agregação, como SUM e COUNT, ficam em `aggregate.rs`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarFunction {
    Upper,
    Lower,
    Length,
    Abs,
    Coalesce,
}

impl ScalarFunction {
    pub fn from_name(name: &str) -> Option<ScalarFunction> {
        Some(match name {
            "UPPER" => ScalarFunction::Upper,
            "LOWER" => ScalarFunction::Lower,
            "LENGTH" => ScalarFunction::Length,
            "ABS" => ScalarFunction::Abs,
            "COALESCE" => ScalarFunction::Coalesce,
            _ => return None,
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            ScalarFunction::Upper => "UPPER",
            ScalarFunction::Lower => "LOWER",
            ScalarFunction::Length => "LENGTH",
            ScalarFunction::Abs => "ABS",
            ScalarFunction::Coalesce => "COALESCE",
        }
    }

    /// Quantos argumentos a função aceita: `(mínimo, máximo)`.
    pub fn arity(&self) -> (usize, usize) {
        match self {
            ScalarFunction::Coalesce => (1, usize::MAX),
            _ => (1, 1),
        }
    }
}

impl BoundExpr {
    /// Calcula o valor da expressão para uma linha.
    pub fn eval(&self, row: &[Value]) -> Result<Value> {
        match self {
            BoundExpr::Literal(v) => Ok(v.clone()),
            BoundExpr::Column { index, .. } => Ok(row[*index].clone()),
            BoundExpr::Unary { op, expr } => eval_unary(*op, expr.eval(row)?),
            // AND e OR avaliam o lado direito só quando precisam
            // (*short-circuit*).
            BoundExpr::Binary {
                op: BinaryOp::And,
                left,
                right,
            } => {
                let l = as_bool(left.eval(row)?)?;
                if l == Some(false) {
                    return Ok(Value::Boolean(false));
                }
                let r = as_bool(right.eval(row)?)?;
                Ok(match (l, r) {
                    (_, Some(false)) => Value::Boolean(false),
                    (Some(true), Some(true)) => Value::Boolean(true),
                    _ => Value::Null,
                })
            }
            BoundExpr::Binary {
                op: BinaryOp::Or,
                left,
                right,
            } => {
                let l = as_bool(left.eval(row)?)?;
                if l == Some(true) {
                    return Ok(Value::Boolean(true));
                }
                let r = as_bool(right.eval(row)?)?;
                Ok(match (l, r) {
                    (_, Some(true)) => Value::Boolean(true),
                    (Some(false), Some(false)) => Value::Boolean(false),
                    _ => Value::Null,
                })
            }
            BoundExpr::Binary { op, left, right } => eval_binary(*op, left.eval(row)?, right.eval(row)?),
            BoundExpr::IsNull { expr, negated } => {
                let is_null = expr.eval(row)?.is_null();
                Ok(Value::Boolean(is_null != *negated))
            }
            BoundExpr::InList { expr, list, negated } => {
                let value = expr.eval(row)?;
                if value.is_null() {
                    return Ok(Value::Null);
                }
                let mut saw_null = false;
                for item in list {
                    match value.sql_cmp(&item.eval(row)?)? {
                        Some(Ordering::Equal) => return Ok(Value::Boolean(!negated)),
                        None => saw_null = true,
                        _ => {}
                    }
                }
                // `1 IN (2, NULL)` é desconhecido: o NULL poderia ser 1.
                Ok(if saw_null {
                    Value::Null
                } else {
                    Value::Boolean(*negated)
                })
            }
            BoundExpr::Function { func, args } => {
                let values = args.iter().map(|a| a.eval(row)).collect::<Result<Vec<_>>>()?;
                eval_function(*func, values)
            }
        }
    }

    /// Avalia uma condição (WHERE, ON, HAVING): só `TRUE` conta como
    /// verdadeiro. FALSE e NULL descartam a linha.
    pub fn eval_condition(&self, row: &[Value]) -> Result<bool> {
        Ok(as_bool(self.eval(row)?)? == Some(true))
    }
}

/// Converte um valor em booleano de três estados (`None` = NULL).
fn as_bool(value: Value) -> Result<Option<bool>> {
    match value {
        Value::Boolean(b) => Ok(Some(b)),
        Value::Null => Ok(None),
        other => Err(DbError::Execution(format!(
            "esperava um booleano, recebi {other}"
        ))),
    }
}

fn eval_unary(op: UnaryOp, value: Value) -> Result<Value> {
    match (op, value) {
        (_, Value::Null) => Ok(Value::Null),
        (UnaryOp::Neg, Value::Integer(i)) => i
            .checked_neg()
            .map(Value::Integer)
            .ok_or_else(|| DbError::Execution("estouro de inteiro".into())),
        (UnaryOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
        (UnaryOp::Not, Value::Boolean(b)) => Ok(Value::Boolean(!b)),
        (op, v) => Err(DbError::Execution(format!("operador {op:?} não se aplica a {v}"))),
    }
}

fn eval_binary(op: BinaryOp, left: Value, right: Value) -> Result<Value> {
    use BinaryOp::*;
    // Quase toda operação com NULL dá NULL.
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    match op {
        Eq | NotEq | Lt | LtEq | Gt | GtEq => {
            let ord = left.sql_cmp(&right)?.expect("NULL já foi tratado");
            Ok(Value::Boolean(match op {
                Eq => ord == Ordering::Equal,
                NotEq => ord != Ordering::Equal,
                Lt => ord == Ordering::Less,
                LtEq => ord != Ordering::Greater,
                Gt => ord == Ordering::Greater,
                GtEq => ord != Ordering::Less,
                _ => unreachable!(),
            }))
        }
        Add | Sub | Mul | Div | Mod => eval_arithmetic(op, left, right),
        Concat => Ok(Value::Text(format!("{left}{right}"))),
        Like => match (&left, &right) {
            (Value::Text(text), Value::Text(pattern)) => Ok(Value::Boolean(like(text, pattern))),
            _ => Err(DbError::Execution("LIKE só funciona com textos".into())),
        },
        And | Or => unreachable!("tratados em eval com short-circuit"),
    }
}

fn eval_arithmetic(op: BinaryOp, left: Value, right: Value) -> Result<Value> {
    use BinaryOp::*;
    let overflow = || DbError::Execution("estouro de inteiro".into());
    match (left, right) {
        // Inteiro com inteiro: aritmética inteira, com checagem de estouro.
        (Value::Integer(a), Value::Integer(b)) => {
            if matches!(op, Div | Mod) && b == 0 {
                return Err(DbError::Execution("divisão por zero".into()));
            }
            let r = match op {
                Add => a.checked_add(b),
                Sub => a.checked_sub(b),
                Mul => a.checked_mul(b),
                Div => a.checked_div(b),
                Mod => a.checked_rem(b),
                _ => unreachable!(),
            };
            r.map(Value::Integer).ok_or_else(overflow)
        }
        // Se um dos lados é FLOAT, tudo vira FLOAT.
        (l, r) => {
            let (Some(a), Some(b)) = (as_f64(&l), as_f64(&r)) else {
                return Err(DbError::Execution(format!(
                    "operação {op} inválida entre {l} e {r}"
                )));
            };
            if matches!(op, Div | Mod) && b == 0.0 {
                return Err(DbError::Execution("divisão por zero".into()));
            }
            Ok(Value::Float(match op {
                Add => a + b,
                Sub => a - b,
                Mul => a * b,
                Div => a / b,
                Mod => a % b,
                _ => unreachable!(),
            }))
        }
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Float(x) => Some(*x),
        _ => None,
    }
}

/// Casamento de padrões do LIKE: `%` casa qualquer sequência (inclusive
/// vazia) e `_` casa exatamente um caractere.
///
/// Algoritmo recursivo direto: para `%`, tentamos "consumir" 0, 1, 2...
/// caracteres do texto. É exponencial no pior caso. Implementações reais
/// usam programação dinâmica ou convertem o padrão num autômato.
pub fn like(text: &str, pattern: &str) -> bool {
    fn go(t: &[char], p: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('%') => (0..=t.len()).any(|skip| go(&t[skip..], &p[1..])),
            Some('_') => !t.is_empty() && go(&t[1..], &p[1..]),
            Some(c) => t.first() == Some(c) && go(&t[1..], &p[1..]),
        }
    }
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    go(&t, &p)
}

fn eval_function(func: ScalarFunction, mut args: Vec<Value>) -> Result<Value> {
    if func == ScalarFunction::Coalesce {
        // Primeiro argumento que não é NULL.
        return Ok(args.into_iter().find(|v| !v.is_null()).unwrap_or(Value::Null));
    }
    let arg = args.swap_remove(0);
    match (func, arg) {
        (_, Value::Null) => Ok(Value::Null),
        (ScalarFunction::Upper, Value::Text(s)) => Ok(Value::Text(s.to_uppercase())),
        (ScalarFunction::Lower, Value::Text(s)) => Ok(Value::Text(s.to_lowercase())),
        (ScalarFunction::Length, Value::Text(s)) => Ok(Value::Integer(s.chars().count() as i64)),
        (ScalarFunction::Abs, Value::Integer(i)) => Ok(Value::Integer(i.abs())),
        (ScalarFunction::Abs, Value::Float(x)) => Ok(Value::Float(x.abs())),
        (f, v) => Err(DbError::Execution(format!(
            "{}() não aceita o valor {v}",
            f.name()
        ))),
    }
}

// ----------------------------------------------------------------------
// Display (usado pelo EXPLAIN)
// ----------------------------------------------------------------------

impl fmt::Display for BoundExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BoundExpr::Literal(v) => fmt_literal(v, f),
            BoundExpr::Column { name, .. } => f.write_str(name),
            BoundExpr::Unary {
                op: UnaryOp::Neg,
                expr,
            } => write!(f, "-{expr}"),
            BoundExpr::Unary {
                op: UnaryOp::Not,
                expr,
            } => write!(f, "NOT {expr}"),
            BoundExpr::Binary { op, left, right } => {
                write_operand(left, f)?;
                write!(f, " {op} ")?;
                write_operand(right, f)
            }
            BoundExpr::IsNull { expr, negated } => {
                write!(f, "{expr} IS {}NULL", if *negated { "NOT " } else { "" })
            }
            BoundExpr::InList { expr, list, negated } => {
                let items: Vec<String> = list.iter().map(|e| e.to_string()).collect();
                let not = if *negated { "NOT " } else { "" };
                write!(f, "{expr} {not}IN ({})", items.join(", "))
            }
            BoundExpr::Function { func, args } => {
                let items: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                write!(f, "{}({})", func.name(), items.join(", "))
            }
        }
    }
}

fn write_operand(expr: &BoundExpr, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match expr {
        BoundExpr::Binary { .. } => write!(f, "({expr})"),
        other => write!(f, "{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(v: Value) -> Box<BoundExpr> {
        Box::new(BoundExpr::Literal(v))
    }

    fn bin(op: BinaryOp, l: Value, r: Value) -> Value {
        BoundExpr::Binary {
            op,
            left: lit(l),
            right: lit(r),
        }
        .eval(&[])
        .unwrap()
    }

    #[test]
    fn logica_de_tres_valores() {
        use Value::{Boolean as B, Null};
        assert_eq!(bin(BinaryOp::And, Null, B(false)), B(false));
        assert!(bin(BinaryOp::And, Null, B(true)).is_null());
        assert_eq!(bin(BinaryOp::Or, Null, B(true)), B(true));
        assert!(bin(BinaryOp::Or, Null, B(false)).is_null());
        assert!(bin(BinaryOp::Eq, Null, Null).is_null());
    }

    #[test]
    fn aritmetica() {
        use Value::{Float, Integer};
        assert_eq!(bin(BinaryOp::Div, Integer(7), Integer(2)), Integer(3));
        assert_eq!(bin(BinaryOp::Div, Integer(7), Float(2.0)), Float(3.5));
        let div0 = BoundExpr::Binary {
            op: BinaryOp::Div,
            left: lit(Integer(1)),
            right: lit(Integer(0)),
        };
        assert!(div0.eval(&[]).is_err());
    }

    #[test]
    fn padroes_like() {
        assert!(like("banana", "b%"));
        assert!(like("banana", "%nan%"));
        assert!(like("banana", "b_n_n_"));
        assert!(!like("banana", "b_n"));
        assert!(like("", "%"));
    }
}
