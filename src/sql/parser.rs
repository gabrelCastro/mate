//! Parser: transforma a lista de tokens numa AST.
//!
//! É um **parser descendente recursivo** (*recursive descent*): cada regra
//! da gramática vira uma função. `parse_select` chama `parse_expr`, que
//! chama `parse_or`, que chama `parse_and`... É o tipo de parser mais fácil
//! de escrever à mão, e Postgres e SQLite usam geradores de parser no lugar
//! dele.
//!
//! # Precedência de operadores
//!
//! Em `a OR b AND c`, o AND "gruda" mais forte: `a OR (b AND c)`. No parser
//! descendente, a precedência sai da **ordem das chamadas**. A função de
//! menor precedência chama a de maior:
//!
//! ```text
//!   parse_expr
//!    └─ parse_or            a OR b
//!        └─ parse_and       a AND b
//!            └─ parse_not   NOT a
//!                └─ parse_comparison   a = b, a < b, a IS NULL, a LIKE b, a IN (...)
//!                    └─ parse_additive         a + b, a - b, a || b
//!                        └─ parse_multiplicative   a * b, a / b, a % b
//!                            └─ parse_unary        -a
//!                                └─ parse_primary  literais, colunas, (expr), funções
//! ```
//!
//! Gramática resumida dos comandos aceitos:
//!
//! ```text
//!   CREATE TABLE [IF NOT EXISTS] t (col TIPO [PRIMARY KEY] [NOT NULL] [UNIQUE], ...)
//!   CREATE [UNIQUE] INDEX nome ON t (col)
//!   DROP TABLE [IF EXISTS] t
//!   INSERT INTO t [(cols)] VALUES (exprs), (exprs)...
//!   SELECT [DISTINCT] itens [FROM t [alias] [[INNER|LEFT] JOIN t2 ON expr]...]
//!          [WHERE expr] [GROUP BY exprs] [HAVING expr]
//!          [ORDER BY expr [ASC|DESC], ...] [LIMIT n] [OFFSET n]
//!   UPDATE t SET col = expr, ... [WHERE expr]
//!   DELETE FROM t [WHERE expr]
//!   BEGIN [TRANSACTION] | COMMIT | ROLLBACK
//!   EXPLAIN comando
//! ```

use crate::error::{DbError, Result};
use crate::sql::ast::*;
use crate::sql::lexer::{Keyword, Token, tokenize};
use crate::types::{DataType, Value};

/// Faz o parse de um texto com um ou mais comandos separados por `;`.
pub fn parse(sql: &str) -> Result<Vec<Statement>> {
    let mut parser = Parser {
        tokens: tokenize(sql)?,
        pos: 0,
        params: 0,
    };
    let mut statements = Vec::new();
    loop {
        while parser.consume(&Token::Semicolon) {}
        if parser.peek().is_none() {
            break;
        }
        parser.params = 0; // cada comando numera seus `?` a partir do 0
        statements.push(parser.parse_statement()?);
        // Depois de um comando, só pode vir `;` ou o fim do texto.
        if parser.peek().is_some() && !parser.consume(&Token::Semicolon) {
            return Err(parser.unexpected("';' ou fim do comando"));
        }
    }
    Ok(statements)
}

/// Atalho para quando se espera exatamente um comando.
pub fn parse_one(sql: &str) -> Result<Statement> {
    let mut stmts = parse(sql)?;
    match stmts.len() {
        1 => Ok(stmts.remove(0)),
        n => Err(DbError::Parse(format!("esperava 1 comando, encontrei {n}"))),
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// Quantos `?` já apareceram no comando atual.
    params: usize,
}

impl Parser {
    // ------------------------------------------------------------------
    // Auxiliares para andar pelos tokens
    // ------------------------------------------------------------------

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        tok
    }

    /// Se o próximo token é `tok`, consome e devolve `true`.
    fn consume(&mut self, tok: &Token) -> bool {
        if self.peek() == Some(tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn consume_keyword(&mut self, kw: Keyword) -> bool {
        self.consume(&Token::Keyword(kw))
    }

    fn expect(&mut self, tok: &Token) -> Result<()> {
        if self.consume(tok) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("'{tok}'")))
        }
    }

    fn expect_keyword(&mut self, kw: Keyword) -> Result<()> {
        self.expect(&Token::Keyword(kw))
    }

    fn expect_ident(&mut self) -> Result<String> {
        match self.peek() {
            Some(Token::Ident(name)) => {
                let name = name.clone();
                self.pos += 1;
                Ok(name)
            }
            _ => Err(self.unexpected("um nome")),
        }
    }

    fn expect_integer(&mut self) -> Result<u64> {
        match self.next() {
            Some(Token::Integer(n)) if n >= 0 => Ok(n as u64),
            _ => {
                self.pos -= 1;
                Err(self.unexpected("um número inteiro não negativo"))
            }
        }
    }

    fn unexpected(&self, expected: &str) -> DbError {
        match self.peek() {
            Some(tok) => DbError::Parse(format!("esperava {expected}, encontrei '{tok}'")),
            None => DbError::Parse(format!("esperava {expected}, mas o comando acabou")),
        }
    }

    // ------------------------------------------------------------------
    // Comandos
    // ------------------------------------------------------------------

    fn parse_statement(&mut self) -> Result<Statement> {
        let Some(Token::Keyword(kw)) = self.peek() else {
            return Err(self.unexpected("um comando (SELECT, INSERT, CREATE...)"));
        };
        match kw {
            Keyword::SELECT => Ok(Statement::Select(Box::new(self.parse_select()?))),
            Keyword::INSERT => self.parse_insert(),
            Keyword::UPDATE => self.parse_update(),
            Keyword::DELETE => self.parse_delete(),
            Keyword::CREATE => self.parse_create(),
            Keyword::DROP => self.parse_drop(),
            Keyword::BEGIN => {
                self.pos += 1;
                self.consume_keyword(Keyword::TRANSACTION);
                Ok(Statement::Begin)
            }
            Keyword::COMMIT => {
                self.pos += 1;
                Ok(Statement::Commit)
            }
            Keyword::ROLLBACK => {
                self.pos += 1;
                Ok(Statement::Rollback)
            }
            Keyword::EXPLAIN => {
                self.pos += 1;
                Ok(Statement::Explain(Box::new(self.parse_statement()?)))
            }
            _ => Err(self.unexpected("um comando (SELECT, INSERT, CREATE...)")),
        }
    }

    fn parse_create(&mut self) -> Result<Statement> {
        self.expect_keyword(Keyword::CREATE)?;
        if self.consume_keyword(Keyword::TABLE) {
            return self.parse_create_table();
        }
        let unique = self.consume_keyword(Keyword::UNIQUE);
        if self.consume_keyword(Keyword::INDEX) {
            let name = self.expect_ident()?;
            self.expect_keyword(Keyword::ON)?;
            let table = self.expect_ident()?;
            self.expect(&Token::LeftParen)?;
            let column = self.expect_ident()?;
            self.expect(&Token::RightParen)?;
            return Ok(Statement::CreateIndex(CreateIndex {
                name,
                table,
                column,
                unique,
            }));
        }
        Err(self.unexpected("TABLE ou INDEX"))
    }

    fn parse_create_table(&mut self) -> Result<Statement> {
        let if_not_exists = if self.consume_keyword(Keyword::IF) {
            self.expect_keyword(Keyword::NOT)?;
            self.expect_keyword(Keyword::EXISTS)?;
            true
        } else {
            false
        };
        let name = self.expect_ident()?;
        self.expect(&Token::LeftParen)?;
        let mut columns = Vec::new();
        loop {
            columns.push(self.parse_column_def()?);
            if !self.consume(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RightParen)?;
        Ok(Statement::CreateTable(CreateTable {
            name,
            columns,
            if_not_exists,
        }))
    }

    fn parse_column_def(&mut self) -> Result<ColumnDef> {
        let name = self.expect_ident()?;
        let data_type = self.parse_data_type()?;
        let mut col = ColumnDef {
            name,
            data_type,
            primary_key: false,
            not_null: false,
            unique: false,
        };
        // Restrições podem vir em qualquer ordem.
        loop {
            if self.consume_keyword(Keyword::PRIMARY) {
                self.expect_keyword(Keyword::KEY)?;
                col.primary_key = true;
                col.not_null = true;
                col.unique = true;
            } else if self.consume_keyword(Keyword::NOT) {
                self.expect_keyword(Keyword::NULL)?;
                col.not_null = true;
            } else if self.consume_keyword(Keyword::UNIQUE) {
                col.unique = true;
            } else if self.consume_keyword(Keyword::NULL) {
                // "NULL" explícito: o padrão, nada a fazer.
            } else {
                return Ok(col);
            }
        }
    }

    /// Tipos são identificadores comuns (não palavras reservadas), e
    /// aceitamos alguns sinônimos populares.
    fn parse_data_type(&mut self) -> Result<DataType> {
        let name = self.expect_ident()?;
        let data_type = match name.as_str() {
            "int" | "integer" | "bigint" => DataType::Integer,
            "float" | "real" | "double" => DataType::Float,
            "text" | "varchar" | "string" => DataType::Text,
            "bool" | "boolean" => DataType::Boolean,
            other => return Err(DbError::Parse(format!("tipo desconhecido: {other}"))),
        };
        // Aceita e ignora tamanhos como VARCHAR(255).
        if self.consume(&Token::LeftParen) {
            self.expect_integer()?;
            self.expect(&Token::RightParen)?;
        }
        Ok(data_type)
    }

    fn parse_drop(&mut self) -> Result<Statement> {
        self.expect_keyword(Keyword::DROP)?;
        self.expect_keyword(Keyword::TABLE)?;
        let if_exists = if self.consume_keyword(Keyword::IF) {
            self.expect_keyword(Keyword::EXISTS)?;
            true
        } else {
            false
        };
        let name = self.expect_ident()?;
        Ok(Statement::DropTable { name, if_exists })
    }

    fn parse_insert(&mut self) -> Result<Statement> {
        self.expect_keyword(Keyword::INSERT)?;
        self.expect_keyword(Keyword::INTO)?;
        let table = self.expect_ident()?;
        let columns = if self.consume(&Token::LeftParen) {
            let cols = self.parse_comma_separated(|p| p.expect_ident())?;
            self.expect(&Token::RightParen)?;
            Some(cols)
        } else {
            None
        };
        self.expect_keyword(Keyword::VALUES)?;
        let rows = self.parse_comma_separated(|p| {
            p.expect(&Token::LeftParen)?;
            let values = p.parse_comma_separated(|p| p.parse_expr())?;
            p.expect(&Token::RightParen)?;
            Ok(values)
        })?;
        Ok(Statement::Insert(Insert { table, columns, rows }))
    }

    fn parse_update(&mut self) -> Result<Statement> {
        self.expect_keyword(Keyword::UPDATE)?;
        let table = self.expect_ident()?;
        self.expect_keyword(Keyword::SET)?;
        let assignments = self.parse_comma_separated(|p| {
            let col = p.expect_ident()?;
            p.expect(&Token::Eq)?;
            Ok((col, p.parse_expr()?))
        })?;
        let where_clause = self.parse_where()?;
        Ok(Statement::Update(Update {
            table,
            assignments,
            where_clause,
        }))
    }

    fn parse_delete(&mut self) -> Result<Statement> {
        self.expect_keyword(Keyword::DELETE)?;
        self.expect_keyword(Keyword::FROM)?;
        let table = self.expect_ident()?;
        let where_clause = self.parse_where()?;
        Ok(Statement::Delete(Delete { table, where_clause }))
    }

    fn parse_where(&mut self) -> Result<Option<Expr>> {
        if self.consume_keyword(Keyword::WHERE) {
            Ok(Some(self.parse_expr()?))
        } else {
            Ok(None)
        }
    }

    fn parse_select(&mut self) -> Result<Select> {
        self.expect_keyword(Keyword::SELECT)?;
        let distinct = self.consume_keyword(Keyword::DISTINCT);
        let projection = self.parse_comma_separated(|p| p.parse_select_item())?;

        let from = if self.consume_keyword(Keyword::FROM) {
            Some(self.parse_from()?)
        } else {
            None
        };
        let where_clause = self.parse_where()?;

        let group_by = if self.consume_keyword(Keyword::GROUP) {
            self.expect_keyword(Keyword::BY)?;
            self.parse_comma_separated(|p| p.parse_expr())?
        } else {
            Vec::new()
        };
        let having = if self.consume_keyword(Keyword::HAVING) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        let order_by = if self.consume_keyword(Keyword::ORDER) {
            self.expect_keyword(Keyword::BY)?;
            self.parse_comma_separated(|p| {
                let expr = p.parse_expr()?;
                let descending = if p.consume_keyword(Keyword::DESC) {
                    true
                } else {
                    p.consume_keyword(Keyword::ASC);
                    false
                };
                Ok(OrderBy { expr, descending })
            })?
        } else {
            Vec::new()
        };

        let limit = if self.consume_keyword(Keyword::LIMIT) {
            Some(self.expect_integer()?)
        } else {
            None
        };
        let offset = if self.consume_keyword(Keyword::OFFSET) {
            Some(self.expect_integer()?)
        } else {
            None
        };

        Ok(Select {
            distinct,
            projection,
            from,
            where_clause,
            group_by,
            having,
            order_by,
            limit,
            offset,
        })
    }

    fn parse_select_item(&mut self) -> Result<SelectItem> {
        if self.consume(&Token::Star) {
            return Ok(SelectItem::Wildcard);
        }
        let expr = self.parse_expr()?;
        // O alias pode vir com AS ou direto: `SELECT preco * 2 AS dobro`
        // ou `SELECT preco * 2 dobro`.
        let alias = if self.consume_keyword(Keyword::AS) {
            Some(self.expect_ident()?)
        } else if let Some(Token::Ident(_)) = self.peek() {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(SelectItem::Expr { expr, alias })
    }

    fn parse_from(&mut self) -> Result<FromClause> {
        let table = self.parse_table_ref()?;
        let mut joins = Vec::new();
        loop {
            let kind = if self.consume_keyword(Keyword::JOIN) {
                JoinKind::Inner
            } else if self.consume_keyword(Keyword::INNER) {
                self.expect_keyword(Keyword::JOIN)?;
                JoinKind::Inner
            } else if self.consume_keyword(Keyword::LEFT) {
                self.expect_keyword(Keyword::JOIN)?;
                JoinKind::Left
            } else {
                break;
            };
            let table = self.parse_table_ref()?;
            self.expect_keyword(Keyword::ON)?;
            let on = self.parse_expr()?;
            joins.push(Join { kind, table, on });
        }
        Ok(FromClause { table, joins })
    }

    fn parse_table_ref(&mut self) -> Result<TableRef> {
        let name = self.expect_ident()?;
        let alias = if self.consume_keyword(Keyword::AS) {
            Some(self.expect_ident()?)
        } else if let Some(Token::Ident(_)) = self.peek() {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(TableRef { name, alias })
    }

    /// `item, item, item` (pelo menos um).
    fn parse_comma_separated<T>(&mut self, mut item: impl FnMut(&mut Parser) -> Result<T>) -> Result<Vec<T>> {
        let mut items = vec![item(self)?];
        while self.consume(&Token::Comma) {
            items.push(item(self)?);
        }
        Ok(items)
    }

    // ------------------------------------------------------------------
    // Expressões, da menor para a maior precedência
    // ------------------------------------------------------------------

    fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while self.consume_keyword(Keyword::OR) {
            let right = self.parse_and()?;
            left = binary(BinaryOp::Or, left, right);
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_not()?;
        while self.consume_keyword(Keyword::AND) {
            let right = self.parse_not()?;
            left = binary(BinaryOp::And, left, right);
        }
        Ok(left)
    }

    fn parse_not(&mut self) -> Result<Expr> {
        if self.consume_keyword(Keyword::NOT) {
            let expr = self.parse_not()?;
            return Ok(Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(expr),
            });
        }
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        let left = self.parse_additive()?;

        let op = match self.peek() {
            Some(Token::Eq) => Some(BinaryOp::Eq),
            Some(Token::NotEq) => Some(BinaryOp::NotEq),
            Some(Token::Lt) => Some(BinaryOp::Lt),
            Some(Token::LtEq) => Some(BinaryOp::LtEq),
            Some(Token::Gt) => Some(BinaryOp::Gt),
            Some(Token::GtEq) => Some(BinaryOp::GtEq),
            _ => None,
        };
        if let Some(op) = op {
            self.pos += 1;
            let right = self.parse_additive()?;
            return Ok(binary(op, left, right));
        }

        // expr IS [NOT] NULL
        if self.consume_keyword(Keyword::IS) {
            let negated = self.consume_keyword(Keyword::NOT);
            self.expect_keyword(Keyword::NULL)?;
            return Ok(Expr::IsNull {
                expr: Box::new(left),
                negated,
            });
        }

        // [NOT] LIKE / IN / BETWEEN
        let negated = self.consume_keyword(Keyword::NOT);
        let expr = if self.consume_keyword(Keyword::LIKE) {
            let pattern = self.parse_additive()?;
            binary(BinaryOp::Like, left, pattern)
        } else if self.consume_keyword(Keyword::IN) {
            self.expect(&Token::LeftParen)?;
            let list = self.parse_comma_separated(|p| p.parse_expr())?;
            self.expect(&Token::RightParen)?;
            return Ok(Expr::InList {
                expr: Box::new(left),
                list,
                negated,
            });
        } else if self.consume_keyword(Keyword::BETWEEN) {
            // `x BETWEEN a AND b` é açúcar sintático para `x >= a AND x <= b`.
            let low = self.parse_additive()?;
            self.expect_keyword(Keyword::AND)?;
            let high = self.parse_additive()?;
            binary(
                BinaryOp::And,
                binary(BinaryOp::GtEq, left.clone(), low),
                binary(BinaryOp::LtEq, left, high),
            )
        } else if negated {
            return Err(self.unexpected("LIKE, IN ou BETWEEN depois de NOT"));
        } else {
            return Ok(left);
        };

        Ok(if negated {
            Expr::Unary {
                op: UnaryOp::Not,
                expr: Box::new(expr),
            }
        } else {
            expr
        })
    }

    fn parse_additive(&mut self) -> Result<Expr> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => BinaryOp::Add,
                Some(Token::Minus) => BinaryOp::Sub,
                Some(Token::Concat) => BinaryOp::Concat,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.parse_multiplicative()?;
            left = binary(op, left, right);
        }
    }

    fn parse_multiplicative(&mut self) -> Result<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => BinaryOp::Mul,
                Some(Token::Slash) => BinaryOp::Div,
                Some(Token::Percent) => BinaryOp::Mod,
                _ => return Ok(left),
            };
            self.pos += 1;
            let right = self.parse_unary()?;
            left = binary(op, left, right);
        }
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        if self.consume(&Token::Minus) {
            let expr = self.parse_unary()?;
            // `-5` vira direto o literal -5, e não "negação de 5". Isso
            // simplifica o planner, que procura literais para usar índices.
            return Ok(match expr {
                Expr::Literal(Value::Integer(i)) => Expr::Literal(Value::Integer(-i)),
                Expr::Literal(Value::Float(x)) => Expr::Literal(Value::Float(-x)),
                other => Expr::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(other),
                },
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let Some(tok) = self.next() else {
            return Err(DbError::Parse(
                "esperava uma expressão, mas o comando acabou".into(),
            ));
        };
        match tok {
            Token::Integer(i) => Ok(Expr::Literal(Value::Integer(i))),
            Token::Float(x) => Ok(Expr::Literal(Value::Float(x))),
            Token::String(s) => Ok(Expr::Literal(Value::Text(s))),
            Token::Keyword(Keyword::NULL) => Ok(Expr::Literal(Value::Null)),
            Token::Keyword(Keyword::TRUE) => Ok(Expr::Literal(Value::Boolean(true))),
            Token::Keyword(Keyword::FALSE) => Ok(Expr::Literal(Value::Boolean(false))),
            Token::Param => {
                self.params += 1;
                Ok(Expr::Param(self.params - 1))
            }
            Token::LeftParen => {
                let expr = self.parse_expr()?;
                self.expect(&Token::RightParen)?;
                Ok(expr)
            }
            Token::Ident(name) => {
                // nome(  → chamada de função
                if self.consume(&Token::LeftParen) {
                    return self.parse_function_call(name);
                }
                // tabela.coluna
                if self.consume(&Token::Dot) {
                    let column = self.expect_ident()?;
                    return Ok(Expr::Column {
                        table: Some(name),
                        name: column,
                    });
                }
                Ok(Expr::Column { table: None, name })
            }
            other => {
                self.pos -= 1;
                Err(DbError::Parse(format!(
                    "esperava uma expressão, encontrei '{other}'"
                )))
            }
        }
    }

    /// Chamado logo depois de `nome(`.
    fn parse_function_call(&mut self, name: String) -> Result<Expr> {
        let name = name.to_uppercase();
        // COUNT(*) é representado com a lista de argumentos vazia.
        if self.consume(&Token::Star) {
            self.expect(&Token::RightParen)?;
            if name != "COUNT" {
                return Err(DbError::Parse(format!("{name}(*) não existe; só COUNT(*)")));
            }
            return Ok(Expr::Function { name, args: vec![] });
        }
        let args = self.parse_comma_separated(|p| p.parse_expr())?;
        self.expect(&Token::RightParen)?;
        Ok(Expr::Function { name, args })
    }
}

fn binary(op: BinaryOp, left: Expr, right: Expr) -> Expr {
    Expr::Binary {
        op,
        left: Box::new(left),
        right: Box::new(right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr(sql: &str) -> Expr {
        let Statement::Select(select) = parse_one(&format!("SELECT {sql}")).unwrap() else {
            panic!()
        };
        let SelectItem::Expr { expr, .. } = &select.projection[0] else {
            panic!()
        };
        expr.clone()
    }

    #[test]
    fn precedencia() {
        // O Display coloca parênteses em volta das subexpressões binárias,
        // deixando a árvore visível.
        assert_eq!(expr("1 + 2 * 3").to_string(), "1 + (2 * 3)");
        assert_eq!(expr("(1 + 2) * 3").to_string(), "(1 + 2) * 3");
        assert_eq!(expr("a OR b AND c").to_string(), "a OR (b AND c)");
        assert_eq!(expr("NOT a = 1").to_string(), "NOT a = 1");
        assert_eq!(expr("-x + 1").to_string(), "-x + 1");
        assert_eq!(expr("x BETWEEN 1 AND 5").to_string(), "(x >= 1) AND (x <= 5)");
    }

    #[test]
    fn select_completo() {
        let stmt = parse_one(
            "SELECT DISTINCT u.nome, COUNT(*) AS total
             FROM users u LEFT JOIN pedidos p ON p.user_id = u.id
             WHERE u.idade >= 18 AND u.nome LIKE 'A%'
             GROUP BY u.nome HAVING COUNT(*) > 1
             ORDER BY total DESC, u.nome LIMIT 10 OFFSET 5",
        )
        .unwrap();
        let Statement::Select(s) = stmt else { panic!() };
        assert!(s.distinct);
        assert_eq!(s.projection.len(), 2);
        let from = s.from.unwrap();
        assert_eq!(from.table.alias.as_deref(), Some("u"));
        assert_eq!(from.joins[0].kind, JoinKind::Left);
        assert_eq!(s.group_by.len(), 1);
        assert!(s.having.is_some());
        assert!(s.order_by[0].descending);
        assert!(!s.order_by[1].descending);
        assert_eq!((s.limit, s.offset), (Some(10), Some(5)));
    }

    #[test]
    fn create_table_volta_ao_sql() {
        let sql = "CREATE TABLE users (id INTEGER PRIMARY KEY, nome TEXT NOT NULL, email TEXT UNIQUE)";
        let Statement::CreateTable(ct) = parse_one(sql).unwrap() else {
            panic!()
        };
        assert_eq!(ct.to_string(), sql);
    }

    #[test]
    fn varios_comandos() {
        let stmts = parse("BEGIN; INSERT INTO t VALUES (1, 'a'), (2, 'b'); COMMIT;").unwrap();
        assert_eq!(stmts.len(), 3);
        let Statement::Insert(ins) = &stmts[1] else {
            panic!()
        };
        assert_eq!(ins.rows.len(), 2);
    }

    #[test]
    fn erros_de_sintaxe() {
        assert!(parse("SELECT FROM").is_err());
        assert!(parse("SELECT 1 2").is_err());
        assert!(parse("CREATE TABLE t (a BLOB)").is_err());
        assert!(parse("SUM(*)").is_err());
    }
}
