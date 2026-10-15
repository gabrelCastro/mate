//! WAL (*Write-Ahead Log*): o diário que garante durabilidade e atomicidade.
//!
//! # O problema
//!
//! Uma transação pode alterar várias páginas. Se o processo cair (ou faltar
//! energia) no meio da gravação dessas páginas no arquivo `.db`, o banco
//! fica num estado impossível: metade da transação aplicada e metade não.
//!
//! # A solução: escrever antes num diário
//!
//! Antes de tocar no arquivo do banco, gravamos a **imagem completa** de
//! cada página alterada num arquivo separado (`banco.db-wal`). A última
//! página da transação recebe a marca de *commit*. Só depois de um `fsync`
//! no WAL a transação conta como confirmada, e aí copiamos as páginas para
//! o `.db`.
//!
//! ```text
//!  banco.db-wal
//!  ┌────────────────────────┬────────────────────────┬─────
//!  │ quadro: pág 7          │ quadro: pág 2 (COMMIT) │ ...
//!  │ [cabeçalho][4096 bytes]│ [cabeçalho][4096 bytes]│
//!  └────────────────────────┴────────────────────────┴─────
//! ```
//!
//! Cada **quadro** (*frame*) tem um cabeçalho de 12 bytes:
//!
//! | offset | tamanho | campo                                       |
//! |--------|---------|---------------------------------------------|
//! | 0      | 4       | id da página                                |
//! | 4      | 4       | 1 se é o último quadro da transação, ou 0   |
//! | 8      | 4       | CRC-32 do id, da marca e dos 4096 bytes      |
//!
//! # Recuperação (*recovery*)
//!
//! Ao abrir o banco, lemos o WAL do começo ao fim:
//!
//! - quadros de transações que chegaram ao COMMIT são regravados no `.db`.
//!   Repetir isso é inofensivo, porque é a imagem inteira da página
//!   (a operação é *idempotente*);
//! - quadros sem COMMIT no final são descartados, porque a transação não
//!   terminou;
//! - um quadro com checksum inválido indica gravação interrompida, e a
//!   leitura para ali.
//!
//! Depois de aplicar tudo, damos `fsync` no `.db` e esvaziamos o WAL. Esse
//! passo se chama **checkpoint**.
//!
//! Esse desenho de "imagens de página + quadro de commit" é o mesmo do modo
//! WAL do SQLite. Postgres e MySQL registram mudanças menores (só os bytes
//! alterados ou a operação lógica), o que economiza espaço mas complica
//! bastante a recuperação.

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::Result;
use crate::storage::checksum::Crc32;
use crate::storage::page::{PAGE_SIZE, Page, PageId};

const FRAME_HEADER_SIZE: usize = 12;
const FRAME_SIZE: usize = FRAME_HEADER_SIZE + PAGE_SIZE;

pub struct Wal {
    file: File,
    /// Quantos quadros existem no arquivo. O pager usa isso para decidir
    /// quando fazer checkpoint.
    frame_count: u64,
}

impl Wal {
    pub fn open(path: &Path) -> Result<Wal> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let frame_count = file.metadata()?.len() / FRAME_SIZE as u64;
        Ok(Wal { file, frame_count })
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Grava as páginas de uma transação e marca a última como COMMIT.
    ///
    /// Quando esta função retorna `Ok`, a transação está **durável**:
    /// mesmo que o processo caia logo depois, a recuperação vai reaplicá-la.
    pub fn append_transaction(&mut self, pages: &[(PageId, &Page)]) -> Result<()> {
        if pages.is_empty() {
            return Ok(());
        }
        // Montamos todos os quadros num buffer e fazemos uma única escrita.
        let mut buf = Vec::with_capacity(pages.len() * FRAME_SIZE);
        for (i, (page_id, page)) in pages.iter().enumerate() {
            let is_commit = i == pages.len() - 1;
            write_frame(&mut buf, *page_id, is_commit, page);
        }
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&buf)?;
        // O fsync é o momento exato em que a transação vira "confirmada".
        self.file.sync_all()?;
        self.frame_count += pages.len() as u64;
        Ok(())
    }

    /// Lê o WAL e devolve as páginas de todas as transações confirmadas, na
    /// ordem em que foram gravadas. Se a mesma página aparece duas vezes, a
    /// última versão é a que vale, então basta aplicá-las em ordem.
    pub fn read_committed(&mut self) -> Result<Vec<(PageId, Page)>> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut committed = Vec::new();
        // Quadros da transação atual, ainda sem o quadro de commit.
        let mut pending = Vec::new();
        let mut frame = vec![0u8; FRAME_SIZE];

        loop {
            match self.file.read_exact(&mut frame) {
                Ok(()) => {}
                // Fim do arquivo (ou quadro cortado no meio): acabou.
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            let Some((page_id, is_commit, page)) = parse_frame(&frame) else {
                // Checksum não bate: gravação interrompida. Paramos aqui.
                break;
            };
            pending.push((page_id, page));
            if is_commit {
                committed.append(&mut pending);
            }
        }
        // O que sobrou em `pending` é de uma transação que não chegou ao
        // commit, e por isso é ignorado.
        Ok(committed)
    }

    /// Esvazia o WAL. Só é seguro depois que o `.db` recebeu (com fsync)
    /// todas as páginas que estavam aqui.
    pub fn reset(&mut self) -> Result<()> {
        self.file.set_len(0)?;
        self.file.sync_all()?;
        self.frame_count = 0;
        Ok(())
    }
}

fn frame_checksum(page_id: PageId, is_commit: bool, page: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(&page_id.to_le_bytes());
    crc.update(&(is_commit as u32).to_le_bytes());
    crc.update(page);
    crc.finish()
}

fn write_frame(buf: &mut Vec<u8>, page_id: PageId, is_commit: bool, page: &Page) {
    buf.extend_from_slice(&page_id.to_le_bytes());
    buf.extend_from_slice(&(is_commit as u32).to_le_bytes());
    buf.extend_from_slice(&frame_checksum(page_id, is_commit, page.bytes()).to_le_bytes());
    buf.extend_from_slice(page.bytes());
}

/// Interpreta um quadro. Devolve `None` se o checksum não bate.
fn parse_frame(frame: &[u8]) -> Option<(PageId, bool, Page)> {
    let read_u32 = |off: usize| u32::from_le_bytes(frame[off..off + 4].try_into().unwrap());
    let page_id = read_u32(0);
    let is_commit = read_u32(4) == 1;
    let checksum = read_u32(8);
    let data = &frame[FRAME_HEADER_SIZE..];
    if frame_checksum(page_id, is_commit, data) != checksum {
        return None;
    }
    Some((page_id, is_commit, Page::from_bytes(data)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn page_with(byte: u8) -> Page {
        let mut p = Page::new();
        p.bytes_mut()[0] = byte;
        p
    }

    #[test]
    fn so_transacoes_confirmadas_voltam() {
        let dir = TempDir::new("wal");
        let mut wal = Wal::open(&dir.path("t.db-wal")).unwrap();
        let (a, b) = (page_with(1), page_with(2));
        wal.append_transaction(&[(3, &a), (4, &b)]).unwrap();

        // Simula uma transação interrompida: um quadro sem a marca de
        // commit, gravado "na mão".
        let mut buf = Vec::new();
        write_frame(&mut buf, 5, false, &page_with(9));
        wal.file.seek(SeekFrom::End(0)).unwrap();
        wal.file.write_all(&buf).unwrap();

        let pages = wal.read_committed().unwrap();
        let ids: Vec<PageId> = pages.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![3, 4]);
        assert_eq!(pages[1].1.bytes()[0], 2);
    }

    #[test]
    fn quadro_corrompido_interrompe_a_leitura() {
        let dir = TempDir::new("wal-corrupt");
        let path = dir.path("t.db-wal");
        let mut wal = Wal::open(&path).unwrap();
        wal.append_transaction(&[(1, &page_with(1))]).unwrap();
        wal.append_transaction(&[(2, &page_with(2))]).unwrap();

        // Estraga um byte do segundo quadro, como uma gravação pela metade.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[FRAME_SIZE + FRAME_HEADER_SIZE + 10] ^= 0xFF;
        std::fs::write(&path, bytes).unwrap();

        let mut wal = Wal::open(&path).unwrap();
        let pages = wal.read_committed().unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].0, 1);
    }
}
