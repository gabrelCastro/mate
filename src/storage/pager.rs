//! Pager: a porta de entrada para todas as páginas do banco.
//!
//! As camadas de cima (heap, B+Tree, catálogo) nunca falam com o disco
//! diretamente. Elas pedem páginas ao pager com `read`, `write` e
//! `allocate`, e ele cuida de três coisas:
//!
//! 1. **Cache** (*buffer pool*): páginas lidas recentemente ficam em memória
//!    num [`LruCache`].
//! 2. **Transações**: páginas alteradas vão para o conjunto `dirty`, que fica
//!    só na memória. O disco não muda até o `commit`, e o `rollback` só
//!    precisa jogar esse conjunto fora.
//! 3. **Durabilidade**: no `commit`, as páginas sujas vão primeiro para o
//!    [`Wal`] (com fsync) e só depois para o arquivo do banco.
//!
//! ```text
//!               read / write / allocate
//!   heap, btree ───────────────────────► Pager
//!                                          │
//!                    ┌─────────────────────┼──────────────────────┐
//!                    ▼                     ▼                      ▼
//!            dirty (BTreeMap)      cache LRU (limpas)       DiskManager
//!            alteradas pela        cópias do que está        arquivo .db
//!            transação atual       no disco                      ▲
//!                    │                                           │
//!                    └──── commit: 1) Wal + fsync  2) grava no .db
//! ```
//!
//! # Política "no-steal"
//!
//! Páginas alteradas por uma transação em andamento **nunca** são gravadas
//! no `.db` antes do commit (em inglês, *no-steal*). Assim o rollback não
//! precisa desfazer nada no disco. O custo é que uma transação precisa
//! caber na memória. Bancos maiores permitem *steal* e guardam no log
//! informação para desfazer (*undo*), mas isso complica muito a recuperação
//! (veja o algoritmo ARIES).
//!
//! # Página 0: o cabeçalho
//!
//! | offset | tamanho | campo                               |
//! |--------|---------|-------------------------------------|
//! | 0      | 8       | assinatura `MATEDB01`               |
//! | 8      | 4       | número total de páginas do banco    |
//!
//! O cabeçalho é uma página como qualquer outra, então alocar páginas
//! também é transacional: num rollback, a contagem volta sozinha.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{DbError, Result};
use crate::storage::disk::DiskManager;
use crate::storage::lru::LruCache;
use crate::storage::page::{Page, PageId};
use crate::storage::wal::Wal;

pub const HEADER_PAGE_ID: PageId = 0;
const MAGIC: &[u8; 8] = b"MATEDB01";
const PAGE_COUNT_OFFSET: usize = 8;

/// Quantas páginas limpas mantemos em memória (256 × 4 KB = 1 MB).
const CACHE_CAPACITY: usize = 256;

/// Quando o WAL passar desse número de quadros, fazemos checkpoint.
const CHECKPOINT_FRAMES: u64 = 1000;

/// Contadores para observar o que o pager está fazendo (comando `.stats`).
#[derive(Debug, Default, Clone)]
pub struct PagerStats {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub disk_reads: u64,
    pub disk_writes: u64,
    pub wal_frames_written: u64,
    pub checkpoints: u64,
    pub recovered_pages: u64,
}

pub struct Pager {
    disk: DiskManager,
    wal: Wal,
    cache: LruCache,
    /// Páginas alteradas pela transação atual. É um `BTreeMap` para que o
    /// commit as grave em ordem de id.
    dirty: BTreeMap<PageId, Page>,
    /// Número de páginas do banco, incluindo as alocadas na transação atual.
    page_count: u32,
    stats: PagerStats,
}

impl Pager {
    /// Abre (ou cria) o banco em `path`. O WAL fica em `<path>-wal`.
    ///
    /// Antes de tudo roda a recuperação: se o processo anterior caiu, as
    /// transações confirmadas que estavam só no WAL são aplicadas agora.
    pub fn open(path: &Path) -> Result<Pager> {
        let mut pager = Pager {
            disk: DiskManager::open(path)?,
            wal: Wal::open(&wal_path(path))?,
            cache: LruCache::new(CACHE_CAPACITY),
            dirty: BTreeMap::new(),
            page_count: 0,
            stats: PagerStats::default(),
        };
        pager.recover()?;

        if pager.disk.num_pages()? == 0 {
            // Arquivo vazio: banco novo. Criamos o cabeçalho e confirmamos.
            let mut header = Page::new();
            header.bytes_mut()[..8].copy_from_slice(MAGIC);
            header.write_u32(PAGE_COUNT_OFFSET, 1);
            pager.page_count = 1;
            pager.dirty.insert(HEADER_PAGE_ID, header);
            pager.commit()?;
        } else {
            let header = pager.disk.read_page(HEADER_PAGE_ID)?;
            if &header.bytes()[..8] != MAGIC {
                return Err(DbError::Corrupt(
                    "o arquivo não é um banco mate (assinatura inválida)".into(),
                ));
            }
            pager.page_count = header.read_u32(PAGE_COUNT_OFFSET);
        }
        Ok(pager)
    }

    /// Reaplica as transações confirmadas que estão no WAL (veja `wal.rs`).
    fn recover(&mut self) -> Result<()> {
        let pages = self.wal.read_committed()?;
        for (id, page) in &pages {
            self.disk.write_page(*id, page)?;
        }
        self.stats.recovered_pages = pages.len() as u64;
        self.disk.sync()?;
        self.wal.reset()
    }

    pub fn page_count(&self) -> u32 {
        self.page_count
    }

    pub fn stats(&self) -> &PagerStats {
        &self.stats
    }

    /// Há alterações ainda não confirmadas?
    pub fn has_uncommitted_changes(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Lê uma página. Ordem de busca: alterações da transação atual, depois
    /// o cache e, por último, o disco.
    ///
    /// Devolve uma referência. Para alterar a página, quem chama faz
    /// `.clone()`, modifica a cópia e entrega de volta com [`Pager::write`].
    pub fn read(&mut self, id: PageId) -> Result<&Page> {
        if id >= self.page_count {
            return Err(DbError::Corrupt(format!(
                "página {id} não existe (o banco tem {} páginas)",
                self.page_count
            )));
        }
        if self.dirty.contains_key(&id) {
            return Ok(&self.dirty[&id]);
        }
        if self.cache.contains(id) {
            self.stats.cache_hits += 1;
        } else {
            self.stats.cache_misses += 1;
            self.stats.disk_reads += 1;
            let page = self.disk.read_page(id)?;
            self.cache.insert(id, page);
        }
        Ok(self
            .cache
            .get(id)
            .expect("a página acabou de ser colocada no cache"))
    }

    /// Registra a nova versão de uma página. Ela só vai para o disco no
    /// commit.
    pub fn write(&mut self, id: PageId, page: Page) -> Result<()> {
        if id >= self.page_count {
            return Err(DbError::Corrupt(format!("escrita na página inexistente {id}")));
        }
        // A cópia do cache ficou velha. A versão atual passa a ser a suja.
        self.cache.remove(id);
        self.dirty.insert(id, page);
        Ok(())
    }

    /// Reserva uma página nova, zerada, no fim do arquivo.
    ///
    /// Simplificação: páginas liberadas (por DROP TABLE, por exemplo) nunca
    /// são reaproveitadas. Bancos reais mantêm uma *free list* de páginas
    /// livres. Fica como exercício (veja `docs/exercicios.md`).
    pub fn allocate(&mut self) -> Result<PageId> {
        let id = self.page_count;
        self.page_count += 1;

        let mut header = self.read(HEADER_PAGE_ID)?.clone();
        header.write_u32(PAGE_COUNT_OFFSET, self.page_count);
        self.write(HEADER_PAGE_ID, header)?;

        self.dirty.insert(id, Page::new());
        Ok(id)
    }

    /// Confirma a transação atual.
    ///
    /// 1. Grava todas as páginas sujas no WAL e dá fsync. A partir daqui a
    ///    transação está garantida.
    /// 2. Copia as páginas para o arquivo do banco, sem fsync. Se o processo
    ///    cair no meio, a recuperação refaz este passo a partir do WAL.
    /// 3. De vez em quando, faz checkpoint para o WAL não crescer sem fim.
    pub fn commit(&mut self) -> Result<()> {
        if self.dirty.is_empty() {
            return Ok(());
        }
        let frames: Vec<(PageId, &Page)> = self.dirty.iter().map(|(id, p)| (*id, p)).collect();
        self.wal.append_transaction(&frames)?;
        self.stats.wal_frames_written += frames.len() as u64;

        for (id, page) in std::mem::take(&mut self.dirty) {
            self.disk.write_page(id, &page)?;
            self.stats.disk_writes += 1;
            self.cache.insert(id, page);
        }

        if self.wal.frame_count() >= CHECKPOINT_FRAMES {
            self.checkpoint()?;
        }
        Ok(())
    }

    /// Desfaz a transação atual: basta esquecer as páginas sujas.
    pub fn rollback(&mut self) -> Result<()> {
        self.dirty.clear();
        // A contagem de páginas volta ao valor confirmado no cabeçalho.
        self.page_count = self.read(HEADER_PAGE_ID)?.read_u32(PAGE_COUNT_OFFSET);
        Ok(())
    }

    /// Garante que o `.db` tem tudo (fsync) e esvazia o WAL.
    pub fn checkpoint(&mut self) -> Result<()> {
        self.disk.sync()?;
        self.wal.reset()?;
        self.stats.checkpoints += 1;
        Ok(())
    }

    /// Só para testes: grava a transação no WAL e "derruba o processo"
    /// antes de tocar no `.db`. O pager é destruído sem rodar o `Drop`
    /// (que faria checkpoint), simulando uma queda de energia.
    #[cfg(test)]
    pub fn commit_and_crash(mut self) {
        let frames: Vec<(PageId, &Page)> = self.dirty.iter().map(|(id, p)| (*id, p)).collect();
        self.wal.append_transaction(&frames).unwrap();
        std::mem::forget(self);
    }
}

impl Drop for Pager {
    /// Ao fechar o banco, fazemos um último checkpoint (melhor esforço).
    /// Alterações não confirmadas são simplesmente descartadas.
    fn drop(&mut self) {
        let _ = self.checkpoint();
    }
}

fn wal_path(db_path: &Path) -> PathBuf {
    let mut s = db_path.as_os_str().to_owned();
    s.push("-wal");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn page_with(byte: u8) -> Page {
        let mut p = Page::new();
        p.bytes_mut()[100] = byte;
        p
    }

    #[test]
    fn commit_persiste_entre_aberturas() {
        let dir = TempDir::new("pager-commit");
        let path = dir.path("t.db");
        {
            let mut pager = Pager::open(&path).unwrap();
            let id = pager.allocate().unwrap();
            assert_eq!(id, 1);
            pager.write(id, page_with(42)).unwrap();
            pager.commit().unwrap();
        }
        let mut pager = Pager::open(&path).unwrap();
        assert_eq!(pager.page_count(), 2);
        assert_eq!(pager.read(1).unwrap().bytes()[100], 42);
    }

    #[test]
    fn rollback_descarta_alteracoes_e_alocacoes() {
        let dir = TempDir::new("pager-rollback");
        let mut pager = Pager::open(&dir.path("t.db")).unwrap();
        let id = pager.allocate().unwrap();
        pager.write(id, page_with(7)).unwrap();
        pager.commit().unwrap();

        pager.write(id, page_with(99)).unwrap();
        pager.allocate().unwrap();
        assert_eq!(pager.page_count(), 3);
        pager.rollback().unwrap();

        assert_eq!(pager.page_count(), 2);
        assert_eq!(pager.read(id).unwrap().bytes()[100], 7);
    }

    #[test]
    fn recuperacao_apos_queda() {
        let dir = TempDir::new("pager-crash");
        let path = dir.path("t.db");
        {
            let mut pager = Pager::open(&path).unwrap();
            let id = pager.allocate().unwrap();
            pager.write(id, page_with(5)).unwrap();
            // A transação vai para o WAL, mas o processo "cai" antes de
            // gravar no .db.
            pager.commit_and_crash();
        }
        // O .db ainda não tem a página 1...
        let db_len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(db_len, crate::storage::page::PAGE_SIZE as u64);

        // ...mas, ao reabrir, a recuperação aplica o WAL.
        let mut pager = Pager::open(&path).unwrap();
        // 3 quadros: o cabeçalho do banco novo (1º commit) e, na transação
        // interrompida, o cabeçalho atualizado + a página 1.
        assert_eq!(pager.stats().recovered_pages, 3);
        assert_eq!(pager.page_count(), 2);
        assert_eq!(pager.read(1).unwrap().bytes()[100], 5);
    }
}
