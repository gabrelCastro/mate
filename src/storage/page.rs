//! A página: a unidade de E/S do banco.
//!
//! Bancos de dados não leem "uma linha" do disco. Eles leem blocos de tamanho
//! fixo, chamados **páginas**. O arquivo do banco é um vetor de páginas:
//!
//! ```text
//!  arquivo.db
//!  ┌──────────┬──────────┬──────────┬──────────┬─────
//!  │ página 0 │ página 1 │ página 2 │ página 3 │ ...
//!  └──────────┴──────────┴──────────┴──────────┴─────
//!  offset:  0       4096       8192      12288
//! ```
//!
//! A página `n` começa no byte `n * PAGE_SIZE`. Por isso um `PageId` (só um
//! número) basta para achar qualquer página.
//!
//! Por que tamanho fixo? Porque o sistema operacional e o disco também
//! trabalham em blocos (geralmente 4 KB). Ler 4 KB custa quase o mesmo que
//! ler 1 byte, então vale a pena aproveitar o bloco inteiro.
//!
//! Esta struct não sabe nada sobre o que tem dentro dos bytes. Quem dá
//! significado a eles são as camadas de cima: a página de cabeçalho
//! (`pager.rs`), a página com slots (`heap/slotted_page.rs`) e os nós da
//! B+Tree (`btree/node.rs`).

/// Tamanho de uma página em bytes. 4 KB é o padrão de SQLite e Postgres usa
/// 8 KB.
pub const PAGE_SIZE: usize = 4096;

/// Identificador de uma página: sua posição no arquivo.
pub type PageId = u32;

/// Um bloco de `PAGE_SIZE` bytes.
///
/// Os bytes ficam numa `Box` (no heap) para que mover uma `Page` de um lugar
/// para outro custe só a cópia de um ponteiro, e não de 4096 bytes.
#[derive(Clone)]
pub struct Page {
    data: Box<[u8; PAGE_SIZE]>,
}

impl Page {
    /// Página nova, com todos os bytes zerados.
    pub fn new() -> Page {
        Page {
            data: Box::new([0u8; PAGE_SIZE]),
        }
    }

    /// Cria uma página a partir de um slice de exatamente `PAGE_SIZE` bytes.
    pub fn from_bytes(bytes: &[u8]) -> Page {
        let mut page = Page::new();
        page.data.copy_from_slice(bytes);
        page
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data[..]
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data[..]
    }

    // ------------------------------------------------------------------
    // Leitura e escrita de inteiros.
    //
    // Toda estrutura em disco é, no fim, "tal número no offset tal". Estes
    // auxiliares fazem a conversão entre bytes e inteiros. Usamos sempre
    // little-endian (o byte menos significativo primeiro), que é a ordem
    // nativa dos processadores x86 e ARM.
    // ------------------------------------------------------------------

    pub fn read_u16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.data[offset], self.data[offset + 1]])
    }

    pub fn write_u16(&mut self, offset: usize, value: u16) {
        self.data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    pub fn read_u32(&self, offset: usize) -> u32 {
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&self.data[offset..offset + 4]);
        u32::from_le_bytes(buf)
    }

    pub fn write_u32(&mut self, offset: usize, value: u32) {
        self.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}

impl Default for Page {
    fn default() -> Self {
        Page::new()
    }
}

impl std::fmt::Debug for Page {
    // Imprimir 4096 bytes não ajuda ninguém; mostramos só o começo.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Page({:02x?}...)", &self.data[..16])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_e_escreve_inteiros() {
        let mut page = Page::new();
        page.write_u16(10, 0xBEEF);
        page.write_u32(100, 123_456_789);
        assert_eq!(page.read_u16(10), 0xBEEF);
        assert_eq!(page.read_u32(100), 123_456_789);
        // Little-endian: o byte menos significativo vem primeiro.
        assert_eq!(page.bytes()[10], 0xEF);
        assert_eq!(page.bytes()[11], 0xBE);
    }
}
