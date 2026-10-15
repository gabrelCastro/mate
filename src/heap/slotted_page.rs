//! Página com slots (*slotted page*): como guardar várias linhas de tamanho
//! variável dentro de uma página.
//!
//! # O layout
//!
//! ```text
//!  0          12                                                  4096
//!  ┌──────────┬────────┬────────┬────────┬─────────────┬────────┬────────┐
//!  │cabeçalho │ slot 0 │ slot 1 │ slot 2 │  espaço     │ linha 2│ linha 0│
//!  │          │        │        │        │  livre      │        │ linha 1│
//!  └──────────┴────────┴────────┴────────┴─────────────┴────────┴────────┘
//!              ──── o vetor de slots cresce ►    ◄── as linhas crescem ───
//!                                                ▲
//!                                             free_end
//! ```
//!
//! - O **vetor de slots** começa logo após o cabeçalho e cresce para a
//!   direita. Cada slot tem 4 bytes: `(offset, tamanho)` da linha.
//! - As **linhas** são gravadas a partir do fim da página e crescem para a
//!   esquerda.
//! - O espaço livre fica no meio. A página está cheia quando os dois lados
//!   se encontram.
//!
//! Uma linha é identificada por `(página, número do slot)`, o
//! [`RecordId`](super::RecordId). A vantagem dessa indireção é que a linha
//! pode **mudar de lugar dentro da página** (na compactação, por exemplo)
//! sem que o `RecordId` mude: basta atualizar o offset no slot.
//!
//! # Cabeçalho (12 bytes)
//!
//! | offset | tamanho | campo                                               |
//! |--------|---------|-----------------------------------------------------|
//! | 0      | 4       | próxima página da tabela (0 = nenhuma)              |
//! | 4      | 4       | última página da tabela (só vale na primeira página) |
//! | 8      | 2       | número de slots                                     |
//! | 10     | 2       | `free_end`: onde começa a área de linhas            |
//!
//! # Remoção
//!
//! Remover uma linha só zera o tamanho no slot (vira uma "lápide"). Os
//! bytes continuam lá e viram espaço fragmentado. Quando uma inserção não
//! cabe no espaço contíguo, mas caberia somando os fragmentos, a página é
//! **compactada**: as linhas vivas são reescritas juntas no fim da página.

use std::ops::{Deref, DerefMut};

use crate::storage::page::{PAGE_SIZE, Page, PageId};

const NEXT_PAGE_OFFSET: usize = 0;
pub const LAST_PAGE_OFFSET: usize = 4;
const SLOT_COUNT_OFFSET: usize = 8;
const FREE_END_OFFSET: usize = 10;
const HEADER_SIZE: usize = 12;
const SLOT_SIZE: usize = 4;

/// Maior linha que cabe numa página (página vazia, com um único slot).
pub const MAX_TUPLE_SIZE: usize = PAGE_SIZE - HEADER_SIZE - SLOT_SIZE;

/// Uma "lente" que interpreta uma [`Page`] como página com slots.
///
/// É genérica sobre `P` para servir tanto para leitura (`P = &Page`) quanto
/// para escrita (`P = &mut Page`) sem duplicar código. Os métodos de
/// leitura exigem só `Deref<Target = Page>`, e os de escrita exigem
/// `DerefMut`.
pub struct SlottedPage<P> {
    page: P,
}

impl<P: Deref<Target = Page>> SlottedPage<P> {
    pub fn new(page: P) -> SlottedPage<P> {
        SlottedPage { page }
    }

    pub fn next_page(&self) -> Option<PageId> {
        match self.page.read_u32(NEXT_PAGE_OFFSET) {
            0 => None, // a página 0 é o cabeçalho do banco, então 0 serve de "nenhuma"
            id => Some(id),
        }
    }

    pub fn last_page(&self) -> PageId {
        self.page.read_u32(LAST_PAGE_OFFSET)
    }

    pub fn slot_count(&self) -> u16 {
        self.page.read_u16(SLOT_COUNT_OFFSET)
    }

    fn free_end(&self) -> usize {
        self.page.read_u16(FREE_END_OFFSET) as usize
    }

    fn slot(&self, slot: u16) -> (usize, usize) {
        let base = HEADER_SIZE + slot as usize * SLOT_SIZE;
        (
            self.page.read_u16(base) as usize,
            self.page.read_u16(base + 2) as usize,
        )
    }

    /// Os bytes da linha no slot, ou `None` se o slot não existe ou foi
    /// removido.
    pub fn get(&self, slot: u16) -> Option<&[u8]> {
        if slot >= self.slot_count() {
            return None;
        }
        match self.slot(slot) {
            (_, 0) => None, // lápide
            (offset, len) => Some(&self.page.bytes()[offset..offset + len]),
        }
    }

    /// Espaço livre contíguo entre o vetor de slots e as linhas.
    fn contiguous_free(&self) -> usize {
        self.free_end() - (HEADER_SIZE + self.slot_count() as usize * SLOT_SIZE)
    }

    /// Espaço livre total, incluindo os buracos deixados por remoções.
    fn total_free(&self) -> usize {
        let used: usize = (0..self.slot_count()).map(|s| self.slot(s).1).sum();
        PAGE_SIZE - HEADER_SIZE - self.slot_count() as usize * SLOT_SIZE - used
    }
}

impl<P: DerefMut<Target = Page>> SlottedPage<P> {
    /// Formata a página como página com slots vazia.
    pub fn init(&mut self) {
        let page = &mut *self.page;
        *page = Page::new();
        page.write_u16(FREE_END_OFFSET, PAGE_SIZE as u16);
    }

    pub fn set_next_page(&mut self, next: Option<PageId>) {
        self.page.write_u32(NEXT_PAGE_OFFSET, next.unwrap_or(0));
    }

    pub fn set_last_page(&mut self, last: PageId) {
        self.page.write_u32(LAST_PAGE_OFFSET, last);
    }

    /// Insere uma linha e devolve o número do slot, ou `None` se não cabe.
    pub fn insert(&mut self, data: &[u8]) -> Option<u16> {
        assert!(!data.is_empty(), "tamanho 0 é reservado para lápides");
        let needed = data.len() + SLOT_SIZE;
        if self.contiguous_free() < needed {
            if self.total_free() < needed {
                return None;
            }
            self.compact();
        }
        let offset = self.free_end() - data.len();
        let slot = self.slot_count();
        self.page.bytes_mut()[offset..offset + data.len()].copy_from_slice(data);
        self.set_slot(slot, offset, data.len());
        self.page.write_u16(SLOT_COUNT_OFFSET, slot + 1);
        self.page.write_u16(FREE_END_OFFSET, offset as u16);
        Some(slot)
    }

    /// Remove a linha do slot. Devolve `false` se ela já não existia.
    pub fn delete(&mut self, slot: u16) -> bool {
        if self.get(slot).is_none() {
            return false;
        }
        self.set_slot(slot, 0, 0);
        true
    }

    fn set_slot(&mut self, slot: u16, offset: usize, len: usize) {
        let base = HEADER_SIZE + slot as usize * SLOT_SIZE;
        self.page.write_u16(base, offset as u16);
        self.page.write_u16(base + 2, len as u16);
    }

    /// Junta as linhas vivas no fim da página, eliminando os buracos.
    /// Os números de slot não mudam, só os offsets.
    fn compact(&mut self) {
        let live: Vec<(u16, Vec<u8>)> = (0..self.slot_count())
            .filter_map(|s| self.get(s).map(|bytes| (s, bytes.to_vec())))
            .collect();
        let mut free_end = PAGE_SIZE;
        for (slot, bytes) in live {
            free_end -= bytes.len();
            self.page.bytes_mut()[free_end..free_end + bytes.len()].copy_from_slice(&bytes);
            self.set_slot(slot, free_end, bytes.len());
        }
        self.page.write_u16(FREE_END_OFFSET, free_end as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_page() -> Page {
        let mut page = Page::new();
        SlottedPage::new(&mut page).init();
        page
    }

    #[test]
    fn insere_le_e_remove() {
        let mut page = empty_page();
        let mut sp = SlottedPage::new(&mut page);
        let a = sp.insert(b"primeira").unwrap();
        let b = sp.insert(b"segunda").unwrap();
        assert_eq!((a, b), (0, 1));
        assert_eq!(sp.get(a), Some(&b"primeira"[..]));
        assert!(sp.delete(a));
        assert_eq!(sp.get(a), None);
        assert_eq!(sp.get(b), Some(&b"segunda"[..]));
        assert!(!sp.delete(a));
    }

    #[test]
    fn enche_a_pagina() {
        let mut page = empty_page();
        let mut sp = SlottedPage::new(&mut page);
        let row = [7u8; 100];
        let mut count = 0;
        while sp.insert(&row).is_some() {
            count += 1;
        }
        // Cada linha ocupa 100 bytes + 4 do slot: (4096 - 12) / 104 = 39.
        assert_eq!(count, 39);
    }

    #[test]
    fn compacta_para_reaproveitar_espaco() {
        let mut page = empty_page();
        let mut sp = SlottedPage::new(&mut page);
        let big = [1u8; 1000];
        let slots: Vec<u16> = (0..4).map(|_| sp.insert(&big).unwrap()).collect();
        assert!(sp.insert(&big).is_none(), "página deveria estar cheia");

        // Remove a linha do meio: sobra um buraco de 1000 bytes.
        sp.delete(slots[1]);
        // 1000 + 4 do novo slot não cabe contíguo, mas cabe após compactar.
        let novo = sp.insert(&[2u8; 900]).unwrap();
        assert_eq!(sp.get(novo).unwrap()[0], 2);
        // As linhas antigas continuam acessíveis pelos mesmos slots.
        assert_eq!(sp.get(slots[0]).unwrap().len(), 1000);
        assert_eq!(sp.get(slots[3]).unwrap().len(), 1000);
    }
}
