//! O resultado de um comando e sua exibição como tabela de texto.

use std::fmt;

use crate::types::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum QueryResult {
    /// Linhas de um SELECT (ou o texto de um EXPLAIN).
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
    /// Quantas linhas um INSERT/UPDATE/DELETE afetou.
    Affected { verb: &'static str, count: u64 },
    /// Confirmação de DDL ou de transação.
    Message(String),
}

impl QueryResult {
    /// Atalho para testes: as linhas, ou um vetor vazio.
    pub fn rows(&self) -> &[Vec<Value>] {
        match self {
            QueryResult::Rows { rows, .. } => rows,
            _ => &[],
        }
    }
}

/// Desenha no estilo do `psql`:
///
/// ```text
///  id | nome
/// ----+-------
///  1  | Ana
///  2  | Bruno
/// (2 linhas)
/// ```
impl fmt::Display for QueryResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryResult::Message(m) => write!(f, "{m}"),
            QueryResult::Affected { verb, count } => {
                let s = if *count == 1 { "linha" } else { "linhas" };
                write!(f, "{verb}: {count} {s}")
            }
            QueryResult::Rows { columns, rows } => {
                let cells: Vec<Vec<String>> = rows
                    .iter()
                    .map(|r| r.iter().map(|v| v.to_string()).collect())
                    .collect();
                // Largura de cada coluna: o maior entre o título e os valores.
                let mut widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
                for row in &cells {
                    for (w, cell) in widths.iter_mut().zip(row) {
                        *w = (*w).max(cell.chars().count());
                    }
                }
                let line = |f: &mut fmt::Formatter<'_>, items: &[String]| -> fmt::Result {
                    let parts: Vec<String> = items
                        .iter()
                        .zip(&widths)
                        .map(|(item, w)| format!(" {item:<w$} "))
                        .collect();
                    writeln!(f, "{}", parts.join("|").trim_end())
                };
                line(f, columns)?;
                let sep: Vec<String> = widths.iter().map(|w| "-".repeat(w + 2)).collect();
                writeln!(f, "{}", sep.join("+"))?;
                for row in &cells {
                    line(f, row)?;
                }
                let s = if rows.len() == 1 { "linha" } else { "linhas" };
                write!(f, "({} {s})", rows.len())
            }
        }
    }
}
