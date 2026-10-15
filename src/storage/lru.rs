//! Cache LRU (*Least Recently Used*) de páginas.
//!
//! Ler do disco é milhares de vezes mais lento que ler da memória. Por isso
//! guardamos as páginas lidas recentemente. Como a memória é limitada, o
//! cache tem uma capacidade máxima, e quando enche alguém precisa sair.
//!
//! A política LRU expulsa a página **usada há mais tempo**. A aposta é que
//! o que foi usado agora há pouco provavelmente será usado de novo em breve
//! (*localidade temporal*).
//!
//! # Implementação
//!
//! Cada acesso incrementa um relógio lógico (`clock`) e carimba a página
//! acessada com o valor atual. Para expulsar, procuramos o menor carimbo
//! com uma busca linear, O(n).
//!
//! Implementações de produção evitam essa busca. Algumas usam uma lista
//! duplamente encadeada junto com o HashMap, para LRU em O(1). Outras usam
//! o algoritmo **CLOCK**, uma aproximação de LRU sem lista (Postgres usa
//! uma variante dele). A busca linear foi escolhida porque é a versão mais
//! fácil de ler e de conferir.

use std::collections::HashMap;

use crate::storage::page::{Page, PageId};

pub struct LruCache {
    capacity: usize,
    clock: u64,
    /// Para cada página: o conteúdo e o "momento" do último acesso.
    entries: HashMap<PageId, (Page, u64)>,
}

impl LruCache {
    pub fn new(capacity: usize) -> LruCache {
        assert!(capacity > 0);
        LruCache {
            capacity,
            clock: 0,
            entries: HashMap::new(),
        }
    }

    pub fn contains(&self, id: PageId) -> bool {
        self.entries.contains_key(&id)
    }

    /// Busca uma página e marca o acesso.
    pub fn get(&mut self, id: PageId) -> Option<&Page> {
        self.clock += 1;
        let now = self.clock;
        self.entries.get_mut(&id).map(|(page, last_used)| {
            *last_used = now;
            &*page
        })
    }

    /// Insere (ou substitui) uma página, expulsando a menos recente se
    /// o cache estiver cheio.
    pub fn insert(&mut self, id: PageId, page: Page) {
        if !self.entries.contains_key(&id) && self.entries.len() >= self.capacity {
            self.evict();
        }
        self.clock += 1;
        self.entries.insert(id, (page, self.clock));
    }

    pub fn remove(&mut self, id: PageId) {
        self.entries.remove(&id);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn evict(&mut self) {
        let victim = self
            .entries
            .iter()
            .min_by_key(|(_, (_, last_used))| *last_used)
            .map(|(id, _)| *id);
        if let Some(id) = victim {
            self.entries.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expulsa_a_menos_usada_recentemente() {
        let mut cache = LruCache::new(2);
        cache.insert(1, Page::new());
        cache.insert(2, Page::new());
        cache.get(1); // 1 passa a ser a mais recente
        cache.insert(3, Page::new()); // cache cheio: sai a 2
        assert!(cache.contains(1));
        assert!(!cache.contains(2));
        assert!(cache.contains(3));
    }
}
