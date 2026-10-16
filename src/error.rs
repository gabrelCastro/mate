//! Tipo de erro único do banco.
//!
//! Todas as camadas devolvem `Result<T>` (um apelido para
//! `std::result::Result<T, DbError>`). Ter um único tipo de erro deixa o
//! operador `?` funcionar em qualquer lugar, e a variante do erro diz em qual
//! etapa algo deu errado: leitura do SQL, planejamento, execução, etc.

use std::fmt;
use std::io;

#[derive(Debug)]
pub enum DbError {
    /// Falha do sistema operacional ao ler/escrever arquivos.
    Io(io::Error),
    /// O arquivo do banco não está no formato esperado (ou foi corrompido).
    Corrupt(String),
    /// O texto SQL não segue a gramática (erro do lexer ou do parser).
    Parse(String),
    /// O SQL é válido, mas não faz sentido: tabela inexistente, coluna
    /// ambígua, tipos incompatíveis... (erro do planner).
    Plan(String),
    /// Erro durante a execução: divisão por zero, tipos errados em tempo de
    /// execução, linha grande demais...
    Execution(String),
    /// Uma restrição foi violada: NOT NULL, UNIQUE, PRIMARY KEY.
    Constraint(String),
    /// Uso incorreto de transações (ex.: COMMIT sem BEGIN).
    Transaction(String),
    /// O arquivo já está aberto por outro processo (no SQLite, SQLITE_BUSY).
    Busy(String),
}

pub type Result<T> = std::result::Result<T, DbError>;

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Io(e) => write!(f, "erro de E/S: {e}"),
            DbError::Corrupt(m) => write!(f, "banco corrompido: {m}"),
            DbError::Parse(m) => write!(f, "erro de sintaxe: {m}"),
            DbError::Plan(m) => write!(f, "erro de planejamento: {m}"),
            DbError::Execution(m) => write!(f, "erro de execução: {m}"),
            DbError::Constraint(m) => write!(f, "restrição violada: {m}"),
            DbError::Transaction(m) => write!(f, "erro de transação: {m}"),
            DbError::Busy(m) => write!(f, "banco ocupado: {m}"),
        }
    }
}

impl std::error::Error for DbError {}

/// Permite usar `?` em chamadas de E/S da biblioteca padrão.
impl From<io::Error> for DbError {
    fn from(e: io::Error) -> Self {
        DbError::Io(e)
    }
}
