//! # SQL: do texto para a AST
//!
//! 1. [`lexer`] — texto → tokens.
//! 2. [`ast`] — as structs que representam um comando.
//! 3. [`parser`] — tokens → AST (parser descendente recursivo).

pub mod ast;
pub mod lexer;
pub mod params;
pub mod parser;

pub use parser::{parse, parse_one};
