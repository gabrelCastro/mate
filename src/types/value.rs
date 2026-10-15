//! Valores e tipos SQL.
//!
//! Todo dado que circula pelo banco (célula de uma tabela, resultado de uma
//! expressão, chave de um índice) é um [`Value`].

use std::cmp::Ordering;
use std::fmt;

use crate::error::{DbError, Result};

/// Os tipos que uma coluna pode ter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    Integer,
    Float,
    Text,
    Boolean,
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            DataType::Integer => "INTEGER",
            DataType::Float => "FLOAT",
            DataType::Text => "TEXT",
            DataType::Boolean => "BOOLEAN",
        };
        f.write_str(name)
    }
}

/// Um valor SQL. `Null` representa a ausência de valor e pode aparecer em
/// qualquer coluna que não seja `NOT NULL`.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Integer(i64),
    Float(f64),
    Text(String),
    Boolean(bool),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// O tipo do valor (`None` para NULL, que não tem tipo).
    pub fn data_type(&self) -> Option<DataType> {
        match self {
            Value::Null => None,
            Value::Integer(_) => Some(DataType::Integer),
            Value::Float(_) => Some(DataType::Float),
            Value::Text(_) => Some(DataType::Text),
            Value::Boolean(_) => Some(DataType::Boolean),
        }
    }

    /// Converte o valor para o tipo de uma coluna, se a conversão for segura.
    ///
    /// INTEGER vira FLOAT sem perda (5 → 5.0). O caminho contrário perderia a
    /// parte fracionária, então recusamos. Este banco é mais rígido que o
    /// SQLite, que aceita quase qualquer coisa em qualquer coluna.
    pub fn coerce_to(self, target: DataType) -> Result<Value> {
        match (self, target) {
            (Value::Null, _) => Ok(Value::Null),
            (Value::Integer(i), DataType::Float) => Ok(Value::Float(i as f64)),
            (v, t) if v.data_type() == Some(t) => Ok(v),
            (v, t) => Err(DbError::Execution(format!(
                "tipo incompatível: esperado {t}, recebido {} ({v})",
                v.data_type().unwrap()
            ))),
        }
    }

    /// Comparação com a semântica do SQL:
    ///
    /// - qualquer comparação com NULL dá "desconhecido" (`Ok(None)`). Por
    ///   isso `NULL = NULL` não é verdadeiro em SQL;
    /// - INTEGER e FLOAT são comparáveis entre si;
    /// - tipos diferentes (texto com número, por exemplo) são um erro.
    pub fn sql_cmp(&self, other: &Value) -> Result<Option<Ordering>> {
        use Value::*;
        Ok(match (self, other) {
            (Null, _) | (_, Null) => None,
            (Integer(a), Integer(b)) => Some(a.cmp(b)),
            (Integer(a), Float(b)) => Some((*a as f64).total_cmp(b)),
            (Float(a), Integer(b)) => Some(a.total_cmp(&(*b as f64))),
            (Float(a), Float(b)) => Some(a.total_cmp(b)),
            (Text(a), Text(b)) => Some(a.cmp(b)),
            (Boolean(a), Boolean(b)) => Some(a.cmp(b)),
            (a, b) => {
                return Err(DbError::Execution(format!(
                    "não é possível comparar {} com {}",
                    a.data_type().unwrap(),
                    b.data_type().unwrap()
                )));
            }
        })
    }

    /// Posição de cada tipo na ordem total (veja `impl Ord`).
    fn type_rank(&self) -> u8 {
        match self {
            Value::Null => 0,
            Value::Boolean(_) => 1,
            Value::Integer(_) | Value::Float(_) => 2,
            Value::Text(_) => 3,
        }
    }
}

/// Ordem **total** entre valores, usada para ordenar (ORDER BY), agrupar
/// (GROUP BY) e organizar as chaves da B+Tree.
///
/// Diferente de [`Value::sql_cmp`], aqui tudo é comparável: NULL vem antes
/// de todos, depois booleanos, números e textos. Estruturas como `BTreeMap`
/// e a própria B+Tree exigem que quaisquer duas chaves sejam comparáveis.
impl Ord for Value {
    fn cmp(&self, other: &Value) -> Ordering {
        match self.type_rank().cmp(&other.type_rank()) {
            Ordering::Equal => self.sql_cmp(other).ok().flatten().unwrap_or(Ordering::Equal), // só NULL × NULL chega aqui
            different => different,
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Value) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A igualdade segue a ordem total: `Integer(1) == Float(1.0)`.
impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Value {}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("NULL"),
            Value::Integer(i) => write!(f, "{i}"),
            // `{:?}` imprime 1.0 em vez de 1, deixando claro que é FLOAT.
            Value::Float(x) => write!(f, "{x:?}"),
            Value::Text(s) => f.write_str(s),
            Value::Boolean(b) => f.write_str(if *b { "TRUE" } else { "FALSE" }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparacao_sql_com_null_e_desconhecida() {
        assert_eq!(Value::Null.sql_cmp(&Value::Null).unwrap(), None);
        assert_eq!(Value::Integer(1).sql_cmp(&Value::Null).unwrap(), None);
    }

    #[test]
    fn inteiros_e_floats_se_comparam() {
        let r = Value::Integer(2).sql_cmp(&Value::Float(2.5)).unwrap();
        assert_eq!(r, Some(Ordering::Less));
        assert_eq!(Value::Integer(1), Value::Float(1.0));
    }

    #[test]
    fn ordem_total() {
        let mut v = vec![
            Value::Text("a".into()),
            Value::Integer(3),
            Value::Null,
            Value::Float(1.5),
            Value::Boolean(true),
        ];
        v.sort();
        assert_eq!(
            v,
            vec![
                Value::Null,
                Value::Boolean(true),
                Value::Float(1.5),
                Value::Integer(3),
                Value::Text("a".into()),
            ]
        );
    }

    #[test]
    fn coercao() {
        assert!(matches!(
            Value::Integer(2).coerce_to(DataType::Float).unwrap(),
            Value::Float(_)
        ));
        assert!(Value::Float(2.5).coerce_to(DataType::Integer).is_err());
        assert!(Value::Text("x".into()).coerce_to(DataType::Integer).is_err());
    }
}
