//! AST (*Abstract Syntax Tree*): o SQL como estrutura de dados.
//!
//! O parser transforma tokens nestas structs. A AST representa o que o
//! usuário **escreveu**, sem checar se faz sentido: `SELECT x FROM nada`
//! vira uma AST válida, e é o planner que descobre que a tabela `nada` não
//! existe.
//!
//! ```text
//!   SELECT nome FROM users WHERE idade > 18
//!
//!   Statement::Select(Select {
//!       projection: [Expr(Column "nome")],
//!       from: users,
//!       where_clause: Binary(Column "idade", Gt, Literal 18),
//!   })
//! ```
//!
//! O `Display` de cada tipo gera SQL de volta. Isso serve para nomear
//! colunas de resultado (`SELECT a + 1` gera a coluna `a + 1`) e para
//! guardar o `CREATE TABLE` no catálogo.

use std::fmt;

use crate::types::{DataType, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    CreateTable(CreateTable),
    CreateIndex(CreateIndex),
    DropTable {
        name: String,
        if_exists: bool,
    },
    Insert(Insert),
    Select(Box<Select>),
    Update(Update),
    Delete(Delete),
    Begin,
    Commit,
    Rollback,
    /// `EXPLAIN <comando>`: mostra o plano em vez de executar.
    Explain(Box<Statement>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateTable {
    pub name: String,
    pub columns: Vec<ColumnDef>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub primary_key: bool,
    pub not_null: bool,
    pub unique: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateIndex {
    pub name: String,
    pub table: String,
    pub column: String,
    pub unique: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    pub table: String,
    /// `INSERT INTO t (a, b)`: as colunas listadas, ou `None` para todas.
    pub columns: Option<Vec<String>>,
    pub rows: Vec<Vec<Expr>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub distinct: bool,
    pub projection: Vec<SelectItem>,
    pub from: Option<FromClause>,
    pub where_clause: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<OrderBy>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectItem {
    /// `*`
    Wildcard,
    /// `expr [AS alias]`
    Expr { expr: Expr, alias: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FromClause {
    pub table: TableRef,
    pub joins: Vec<Join>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableRef {
    pub name: String,
    pub alias: Option<String>,
}

impl TableRef {
    /// O nome pelo qual a tabela é referenciada na consulta.
    pub fn visible_name(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    /// LEFT JOIN: toda linha da esquerda aparece, mesmo sem par na direita
    /// (as colunas da direita vêm NULL).
    Left,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    pub kind: JoinKind,
    pub table: TableRef,
    pub on: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderBy {
    pub expr: Expr,
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub table: String,
    pub assignments: Vec<(String, Expr)>,
    pub where_clause: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Delete {
    pub table: String,
    pub where_clause: Option<Expr>,
}

/// Uma expressão: qualquer coisa que produz um valor.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Value),
    /// `?`: o parâmetro de número `n` (contando do 0, na ordem do texto).
    /// Antes de planejar, `sql::params::bind_params` troca cada um pelo
    /// valor informado.
    Param(usize),
    /// `coluna` ou `tabela.coluna`
    Column {
        table: Option<String>,
        name: String,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `expr IS NULL` / `expr IS NOT NULL`
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
    /// `expr [NOT] IN (a, b, c)`
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    /// Chamada de função: `UPPER(nome)`, `COUNT(*)`, `SUM(preco)`...
    /// Em `COUNT(*)`, `args` fica vazio.
    Function {
        name: String,
        args: Vec<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Concat,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    Like,
}

// ----------------------------------------------------------------------
// Display: de volta para SQL
// ----------------------------------------------------------------------

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::Concat => "||",
            BinaryOp::Eq => "=",
            BinaryOp::NotEq => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::LtEq => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::GtEq => ">=",
            BinaryOp::And => "AND",
            BinaryOp::Or => "OR",
            BinaryOp::Like => "LIKE",
        })
    }
}

/// Escreve um valor como literal SQL (textos entre aspas).
pub fn fmt_literal(value: &Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match value {
        Value::Text(s) => write!(f, "'{}'", s.replace('\'', "''")),
        other => write!(f, "{other}"),
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Literal(v) => fmt_literal(v, f),
            Expr::Column { table: Some(t), name } => write!(f, "{t}.{name}"),
            Expr::Column { table: None, name } => f.write_str(name),
            Expr::Unary {
                op: UnaryOp::Neg,
                expr,
            } => write!(f, "-{expr}"),
            Expr::Unary {
                op: UnaryOp::Not,
                expr,
            } => write!(f, "NOT {expr}"),
            Expr::Binary { op, left, right } => {
                write_operand(left, f)?;
                write!(f, " {op} ")?;
                write_operand(right, f)
            }
            Expr::IsNull { expr, negated } => {
                write!(f, "{expr} IS {}NULL", if *negated { "NOT " } else { "" })
            }
            Expr::InList { expr, list, negated } => {
                let items: Vec<String> = list.iter().map(|e| e.to_string()).collect();
                let not = if *negated { "NOT " } else { "" };
                write!(f, "{expr} {not}IN ({})", items.join(", "))
            }
            Expr::Param(_) => f.write_str("?"),
            Expr::Function { name, args } if args.is_empty() => write!(f, "{name}(*)"),
            Expr::Function { name, args } => {
                let items: Vec<String> = args.iter().map(|e| e.to_string()).collect();
                write!(f, "{name}({})", items.join(", "))
            }
        }
    }
}

/// Operandos que também são operações binárias vão entre parênteses, para
/// que `(a + b) * c` não seja impresso como `a + b * c`.
fn write_operand(expr: &Expr, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match expr {
        Expr::Binary { .. } => write!(f, "({expr})"),
        other => write!(f, "{other}"),
    }
}

impl fmt::Display for CreateTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cols: Vec<String> = self
            .columns
            .iter()
            .map(|c| {
                let mut s = format!("{} {}", c.name, c.data_type);
                if c.primary_key {
                    s.push_str(" PRIMARY KEY");
                }
                if c.not_null && !c.primary_key {
                    s.push_str(" NOT NULL");
                }
                if c.unique && !c.primary_key {
                    s.push_str(" UNIQUE");
                }
                s
            })
            .collect();
        write!(f, "CREATE TABLE {} ({})", self.name, cols.join(", "))
    }
}

impl fmt::Display for CreateIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unique = if self.unique { "UNIQUE " } else { "" };
        write!(
            f,
            "CREATE {unique}INDEX {} ON {} ({})",
            self.name, self.table, self.column
        )
    }
}
