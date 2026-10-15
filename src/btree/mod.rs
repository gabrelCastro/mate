//! # B+Tree: os índices
//!
//! 1. [`node`] — os dois tipos de nó (folha e interno) e seu formato em disco.
//! 2. [`tree`] — busca, inserção com split, remoção e o cursor de varredura.

pub mod node;
pub mod tree;

pub use node::IndexEntry;
pub use tree::{BTree, BTreeCursor};
