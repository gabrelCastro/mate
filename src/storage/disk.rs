//! Gerenciador de disco: lê e grava páginas inteiras no arquivo do banco.
//!
//! É a camada mais baixa do banco e a única que mexe no arquivo `.db`. Ela
//! não tem cache nem transações e não sabe o que há dentro das páginas.
//! Só faz "página N → bytes no offset N * PAGE_SIZE".
//!
//! # Um processo por vez
//!
//! O Mate não coordena acessos simultâneos. Se dois processos abrissem o
//! mesmo arquivo, cada um teria seu próprio cache e seu próprio WAL na
//! cabeça, e um sobrescreveria as páginas do outro, corrompendo o banco.
//! Para impedir isso, ao abrir o arquivo pedimos ao sistema operacional uma
//! **trava exclusiva** (`flock` no Linux, `LockFileEx` no Windows). Um
//! segundo processo que tentar abrir recebe [`DbError::Busy`]. A trava some
//! sozinha quando o arquivo é fechado, inclusive se o processo morrer.
//!
//! Bancos que aceitam vários clientes travam pedaços menores: o SQLite trava
//! faixas de bytes do arquivo, e o Postgres trava linhas e tabelas na
//! memória compartilhada do servidor.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::{DbError, Result};
use crate::storage::page::{PAGE_SIZE, Page, PageId};

pub struct DiskManager {
    file: File,
}

impl DiskManager {
    /// Abre o arquivo do banco, criando-o se não existir, e o trava para uso
    /// exclusivo deste processo.
    pub fn open(path: &Path) -> Result<DiskManager> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(DbError::Busy(format!(
                    "'{}' já está aberto em outro processo",
                    path.display()
                )));
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
        Ok(DiskManager { file })
    }

    /// Só para testes que simulam uma queda: libera a trava sem fechar o
    /// arquivo (um processo que morre de verdade a libera sozinho).
    #[cfg(test)]
    pub fn release_lock(&self) {
        self.file.unlock().unwrap();
    }

    /// Quantas páginas cabem no arquivo hoje.
    pub fn num_pages(&self) -> Result<u32> {
        let len = self.file.metadata()?.len();
        Ok((len / PAGE_SIZE as u64) as u32)
    }

    /// Lê a página `id`. Uma página além do fim do arquivo volta zerada:
    /// ela foi alocada, mas ainda não foi gravada.
    pub fn read_page(&mut self, id: PageId) -> Result<Page> {
        if id >= self.num_pages()? {
            return Ok(Page::new());
        }
        let mut page = Page::new();
        self.file.seek(SeekFrom::Start(offset_of(id)))?;
        self.file.read_exact(page.bytes_mut())?;
        Ok(page)
    }

    /// Grava a página `id` no lugar dela no arquivo.
    ///
    /// Atenção: depois de `write_page` os dados podem ainda estar só no
    /// cache do sistema operacional, e uma queda de energia agora os perderia.
    /// Para ter certeza de que chegaram ao disco, chame [`DiskManager::sync`].
    pub fn write_page(&mut self, id: PageId, page: &Page) -> Result<()> {
        self.file.seek(SeekFrom::Start(offset_of(id)))?;
        self.file.write_all(page.bytes())?;
        Ok(())
    }

    /// Força o sistema operacional a gravar no disco tudo o que foi escrito
    /// (chamada `fsync`). É lenta, mas é a única garantia de durabilidade.
    pub fn sync(&mut self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }
}

fn offset_of(id: PageId) -> u64 {
    id as u64 * PAGE_SIZE as u64
}
