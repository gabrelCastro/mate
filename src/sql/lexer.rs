//! Lexer (analisador léxico): transforma o texto SQL numa lista de tokens.
//!
//! ```text
//!   "SELECT nome FROM users WHERE id >= 10;"
//!        │
//!        ▼
//!   [SELECT] [Ident(nome)] [FROM] [Ident(users)] [WHERE]
//!   [Ident(id)] [>=] [Integer(10)] [;]
//! ```
//!
//! O lexer não sabe nada de gramática: ele só agrupa caracteres em
//! "palavras" com significado. Decidir se a sequência de tokens forma um
//! comando válido é trabalho do parser (`parser.rs`).

use std::fmt;

use crate::error::{DbError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// Palavra reservada: SELECT, FROM, WHERE...
    Keyword(Keyword),
    /// Nome de tabela, coluna, índice ou função.
    Ident(String),
    Integer(i64),
    Float(f64),
    /// Texto entre aspas simples: 'olá'.
    String(String),
    LeftParen,
    RightParen,
    Comma,
    Semicolon,
    Dot,
    Star,
    Plus,
    Minus,
    Slash,
    Percent,
    /// `||`, concatenação de textos.
    Concat,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
}

/// Gera o enum `Keyword` e a função que reconhece uma palavra-chave a partir
/// do texto, sem precisar escrever cada nome duas vezes.
macro_rules! keywords {
    ($($kw:ident),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[allow(clippy::upper_case_acronyms)]
        pub enum Keyword { $($kw),* }

        impl Keyword {
            /// Reconhece uma palavra-chave (maiúsculas e minúsculas dão no mesmo).
            fn from_str(word: &str) -> Option<Keyword> {
                match word.to_ascii_uppercase().as_str() {
                    $(stringify!($kw) => Some(Keyword::$kw),)*
                    _ => None,
                }
            }

            pub fn as_str(&self) -> &'static str {
                match self { $(Keyword::$kw => stringify!($kw)),* }
            }
        }
    };
}

keywords! {
    AND, AS, ASC, BEGIN, BETWEEN, BY, COMMIT, CREATE, DELETE, DESC, DISTINCT,
    DROP, EXISTS, EXPLAIN, FALSE, FROM, GROUP, HAVING, IF, IN, INDEX, INNER,
    INSERT, INTO, IS, JOIN, KEY, LEFT, LIKE, LIMIT, NOT, NULL, OFFSET, ON, OR,
    ORDER, PRIMARY, ROLLBACK, SELECT, SET, TABLE, TRANSACTION, TRUE, UNIQUE,
    UPDATE, VALUES, WHERE,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Keyword(k) => f.write_str(k.as_str()),
            Token::Ident(s) => f.write_str(s),
            Token::Integer(i) => write!(f, "{i}"),
            Token::Float(x) => write!(f, "{x}"),
            Token::String(s) => write!(f, "'{s}'"),
            Token::LeftParen => f.write_str("("),
            Token::RightParen => f.write_str(")"),
            Token::Comma => f.write_str(","),
            Token::Semicolon => f.write_str(";"),
            Token::Dot => f.write_str("."),
            Token::Star => f.write_str("*"),
            Token::Plus => f.write_str("+"),
            Token::Minus => f.write_str("-"),
            Token::Slash => f.write_str("/"),
            Token::Percent => f.write_str("%"),
            Token::Concat => f.write_str("||"),
            Token::Eq => f.write_str("="),
            Token::NotEq => f.write_str("!="),
            Token::Lt => f.write_str("<"),
            Token::LtEq => f.write_str("<="),
            Token::Gt => f.write_str(">"),
            Token::GtEq => f.write_str(">="),
        }
    }
}

/// Quebra o texto SQL em tokens.
pub fn tokenize(sql: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = sql.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        // Espaços e quebras de linha só separam tokens.
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // Comentário de linha: -- até o fim da linha.
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Palavras: palavra-chave ou identificador.
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            tokens.push(match Keyword::from_str(&word) {
                Some(kw) => Token::Keyword(kw),
                // Identificadores não diferenciam maiúsculas: guardamos em
                // minúsculas, como o Postgres faz.
                None => Token::Ident(word.to_lowercase()),
            });
            continue;
        }

        // Identificador entre aspas duplas: "Nome Com Espaço" (mantém o caso).
        if c == '"' {
            let (text, next) = read_quoted(&chars, i, '"')?;
            tokens.push(Token::Ident(text));
            i = next;
            continue;
        }

        // Números: 42, 3.14
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let is_float = chars.get(i) == Some(&'.') && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
            if is_float {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(if is_float {
                Token::Float(text.parse().unwrap())
            } else {
                Token::Integer(
                    text.parse()
                        .map_err(|_| DbError::Parse(format!("número grande demais: {text}")))?,
                )
            });
            continue;
        }

        // Texto: 'abc'. Uma aspa dentro do texto se escreve dobrada: 'it''s'.
        if c == '\'' {
            let (text, next) = read_quoted(&chars, i, '\'')?;
            tokens.push(Token::String(text));
            i = next;
            continue;
        }

        // Símbolos de dois caracteres precisam ser testados antes dos de um.
        let two: String = chars[i..chars.len().min(i + 2)].iter().collect();
        let symbol2 = match two.as_str() {
            "<=" => Some(Token::LtEq),
            ">=" => Some(Token::GtEq),
            "!=" | "<>" => Some(Token::NotEq),
            "||" => Some(Token::Concat),
            _ => None,
        };
        if let Some(tok) = symbol2 {
            tokens.push(tok);
            i += 2;
            continue;
        }

        let symbol = match c {
            '(' => Token::LeftParen,
            ')' => Token::RightParen,
            ',' => Token::Comma,
            ';' => Token::Semicolon,
            '.' => Token::Dot,
            '*' => Token::Star,
            '+' => Token::Plus,
            '-' => Token::Minus,
            '/' => Token::Slash,
            '%' => Token::Percent,
            '=' => Token::Eq,
            '<' => Token::Lt,
            '>' => Token::Gt,
            other => return Err(DbError::Parse(format!("caractere inesperado: '{other}'"))),
        };
        tokens.push(symbol);
        i += 1;
    }
    Ok(tokens)
}

/// Lê um trecho entre aspas a partir de `start` (que aponta para a aspa de
/// abertura). Devolve o conteúdo e a posição logo após a aspa de fechamento.
fn read_quoted(chars: &[char], start: usize, quote: char) -> Result<(String, usize)> {
    let mut text = String::new();
    let mut i = start + 1;
    loop {
        match chars.get(i) {
            None => return Err(DbError::Parse("texto entre aspas não foi fechado".into())),
            Some(&c) if c == quote => {
                // Aspa dobrada é uma aspa literal; aspa sozinha fecha o texto.
                if chars.get(i + 1) == Some(&quote) {
                    text.push(quote);
                    i += 2;
                } else {
                    return Ok((text, i + 1));
                }
            }
            Some(&c) => {
                text.push(c);
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokeniza_um_select() {
        let tokens = tokenize("select Nome from users where id >= 10.5; -- fim").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Keyword(Keyword::SELECT),
                Token::Ident("nome".into()),
                Token::Keyword(Keyword::FROM),
                Token::Ident("users".into()),
                Token::Keyword(Keyword::WHERE),
                Token::Ident("id".into()),
                Token::GtEq,
                Token::Float(10.5),
                Token::Semicolon,
            ]
        );
    }

    #[test]
    fn textos_com_aspas() {
        let tokens = tokenize("'it''s' \"Coluna X\"").unwrap();
        assert_eq!(
            tokens,
            vec![Token::String("it's".into()), Token::Ident("Coluna X".into())]
        );
        assert!(tokenize("'sem fim").is_err());
    }
}
