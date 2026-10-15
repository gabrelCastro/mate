//! # Camada de armazenamento
//!
//! Tudo o que transforma "bytes num arquivo" em "páginas que podem ser lidas,
//! alteradas e confirmadas com segurança". Leia nesta ordem:
//!
//! 1. [`page`] — o bloco de 4 KB e como ler e escrever inteiros nele.
//! 2. [`disk`] — ler e gravar a página N no arquivo.
//! 3. [`lru`] — o cache de páginas.
//! 4. [`checksum`] e [`wal`] — o diário que sobrevive a quedas.
//! 5. [`pager`] — junta tudo e oferece `read`, `write`, `allocate`, `commit`
//!    e `rollback` para o resto do banco.

pub mod checksum;
pub mod disk;
pub mod lru;
pub mod page;
pub mod pager;
pub mod wal;

pub use page::{PAGE_SIZE, Page, PageId};
pub use pager::Pager;
