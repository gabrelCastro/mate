//! # Tipos e serialização
//!
//! - [`value`] — o enum [`Value`] (os dados que circulam pelo banco) e
//!   [`DataType`] (os tipos das colunas).
//! - [`encoding`] — como valores e linhas viram bytes para caber nas páginas.

pub mod encoding;
pub mod value;

pub use value::{DataType, Value};
