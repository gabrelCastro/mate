//! Heap da tabela: onde as linhas de uma tabela realmente moram.
//!
//! Uma tabela é uma **lista encadeada de páginas com slots**, sem ordem
//! nenhuma entre as linhas (por isso o nome *heap*, "monte"):
//!
//! ```text
//!   catálogo: tabela "users" começa na página 5
//!
//!   ┌─────────┐  next  ┌─────────┐  next  ┌─────────┐
//!   │ pág. 5  │ ─────► │ pág. 9  │ ─────► │ pág. 12 │ ──► (nenhuma)
//!   │ last=12 │        │         │        │         │
//!   └─────────┘        └─────────┘        └─────────┘
//! ```
//!
//! A primeira página também guarda qual é a última (`last`). Assim uma
//! inserção vai direto para o fim, sem percorrer a lista.
//!
//! Buscar uma linha por valor exige ler todas as páginas (*sequential
//! scan*). Para evitar isso existem os índices (`btree/`), que guardam
//! `valor → RecordId`.

use crate::error::{DbError, Result};
use crate::heap::slotted_page::{LAST_PAGE_OFFSET, MAX_TUPLE_SIZE, SlottedPage};
use crate::storage::page::{Page, PageId};
use crate::storage::pager::Pager;

/// O "endereço" de uma linha: em qual página e em qual slot ela está.
/// Os índices guardam `RecordId`s para apontar para as linhas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordId {
    pub page_id: PageId,
    pub slot: u16,
}

impl std::fmt::Display for RecordId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {})", self.page_id, self.slot)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TableHeap {
    pub first_page: PageId,
}

impl TableHeap {
    /// Cria um heap vazio (uma página) e devolve-o.
    pub fn create(pager: &mut Pager) -> Result<TableHeap> {
        let id = pager.allocate()?;
        let mut page = Page::new();
        let mut sp = SlottedPage::new(&mut page);
        sp.init();
        sp.set_last_page(id);
        pager.write(id, page)?;
        Ok(TableHeap { first_page: id })
    }

    pub fn open(first_page: PageId) -> TableHeap {
        TableHeap { first_page }
    }

    /// Insere uma linha (já serializada) e devolve seu endereço.
    pub fn insert(&self, pager: &mut Pager, data: &[u8]) -> Result<RecordId> {
        if data.len() > MAX_TUPLE_SIZE {
            return Err(DbError::Execution(format!(
                "linha com {} bytes excede o máximo de {MAX_TUPLE_SIZE} bytes por linha",
                data.len()
            )));
        }

        // 1. Tenta inserir na última página.
        let last_id = pager.read(self.first_page)?.read_u32(LAST_PAGE_OFFSET);
        let mut last = pager.read(last_id)?.clone();
        if let Some(slot) = SlottedPage::new(&mut last).insert(data) {
            pager.write(last_id, last)?;
            return Ok(RecordId {
                page_id: last_id,
                slot,
            });
        }

        // 2. Não coube: cria uma página nova no fim da lista...
        let new_id = pager.allocate()?;
        let mut new_page = Page::new();
        let mut sp = SlottedPage::new(&mut new_page);
        sp.init();
        let slot = sp
            .insert(data)
            .expect("uma linha válida sempre cabe numa página vazia");
        pager.write(new_id, new_page)?;

        // ...liga a antiga última página à nova...
        SlottedPage::new(&mut last).set_next_page(Some(new_id));
        pager.write(last_id, last)?;

        // ...e atualiza o ponteiro "last" na primeira página. Ela é relida
        // porque pode ser a mesma página que acabamos de alterar.
        let mut first = pager.read(self.first_page)?.clone();
        SlottedPage::new(&mut first).set_last_page(new_id);
        pager.write(self.first_page, first)?;

        Ok(RecordId {
            page_id: new_id,
            slot,
        })
    }

    /// Lê a linha em `rid`, ou `None` se ela foi removida.
    pub fn get(&self, pager: &mut Pager, rid: RecordId) -> Result<Option<Vec<u8>>> {
        let page = pager.read(rid.page_id)?;
        Ok(SlottedPage::new(page).get(rid.slot).map(|b| b.to_vec()))
    }

    /// Remove a linha em `rid`. Devolve `false` se ela já não existia.
    pub fn delete(&self, pager: &mut Pager, rid: RecordId) -> Result<bool> {
        let mut page = pager.read(rid.page_id)?.clone();
        let deleted = SlottedPage::new(&mut page).delete(rid.slot);
        if deleted {
            pager.write(rid.page_id, page)?;
        }
        Ok(deleted)
    }

    /// Cursor para percorrer todas as linhas, página por página.
    pub fn scan(&self) -> HeapCursor {
        HeapCursor {
            page_id: Some(self.first_page),
            slot: 0,
        }
    }
}

/// Cursor de varredura sequencial.
///
/// Não guarda referência ao pager: guarda só **onde parou** (página e slot).
/// A cada `next` recebe o pager de novo. Esse padrão de "estado explícito +
/// contexto passado a cada chamada" se repete nos executores e evita brigas
/// com o borrow checker.
pub struct HeapCursor {
    page_id: Option<PageId>,
    slot: u16,
}

impl HeapCursor {
    pub fn next(&mut self, pager: &mut Pager) -> Result<Option<(RecordId, Vec<u8>)>> {
        while let Some(page_id) = self.page_id {
            let sp = SlottedPage::new(pager.read(page_id)?);
            while self.slot < sp.slot_count() {
                let slot = self.slot;
                self.slot += 1;
                if let Some(bytes) = sp.get(slot) {
                    return Ok(Some((RecordId { page_id, slot }, bytes.to_vec())));
                }
            }
            // Acabaram os slots desta página: segue para a próxima.
            self.page_id = sp.next_page();
            self.slot = 0;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    #[test]
    fn insere_varias_paginas_e_percorre() {
        let dir = TempDir::new("heap");
        let mut pager = Pager::open(&dir.path("t.db")).unwrap();
        let heap = TableHeap::create(&mut pager).unwrap();

        // 500 linhas de ~100 bytes não cabem numa página só.
        let mut rids = Vec::new();
        for i in 0..500u32 {
            let mut row = vec![0u8; 100];
            row[..4].copy_from_slice(&i.to_le_bytes());
            rids.push(heap.insert(&mut pager, &row).unwrap());
        }
        assert!(rids.last().unwrap().page_id > heap.first_page);

        // Remove as linhas pares.
        for rid in rids.iter().step_by(2) {
            assert!(heap.delete(&mut pager, *rid).unwrap());
        }

        // A varredura encontra exatamente as ímpares, em ordem.
        let mut cursor = heap.scan();
        let mut seen = Vec::new();
        while let Some((_, bytes)) = cursor.next(&mut pager).unwrap() {
            seen.push(u32::from_le_bytes(bytes[..4].try_into().unwrap()));
        }
        let expected: Vec<u32> = (0..500).filter(|i| i % 2 == 1).collect();
        assert_eq!(seen, expected);

        assert!(heap.get(&mut pager, rids[0]).unwrap().is_none());
        assert!(heap.get(&mut pager, rids[1]).unwrap().is_some());
    }

    #[test]
    fn linha_grande_demais() {
        let dir = TempDir::new("heap-big");
        let mut pager = Pager::open(&dir.path("t.db")).unwrap();
        let heap = TableHeap::create(&mut pager).unwrap();
        assert!(heap.insert(&mut pager, &vec![0u8; 5000]).is_err());
    }
}
