//! # Heap: armazenamento das linhas
//!
//! 1. [`slotted_page`] — como várias linhas de tamanho variável dividem uma
//!    página.
//! 2. [`table_heap`] — uma tabela como lista encadeada dessas páginas, e o
//!    [`RecordId`] que endereça cada linha.

pub mod slotted_page;
pub mod table_heap;

pub use table_heap::{HeapCursor, RecordId, TableHeap};
