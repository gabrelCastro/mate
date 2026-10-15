//! CRC-32: detecta bytes corrompidos ou gravados pela metade.
//!
//! O WAL (`wal.rs`) guarda um checksum em cada registro. Na recuperação,
//! recalculamos o checksum e comparamos. Se não bater, o registro foi
//! interrompido no meio da gravação (o processo caiu) e é descartado.
//!
//! Esta é a versão bit a bit do algoritmo, a mais curta de ler. Bibliotecas
//! reais usam uma tabela pré-calculada de 256 entradas, ou instruções
//! próprias do processador, para ir muito mais rápido.

/// Calcula o CRC-32 (o mesmo usado por zip, PNG e ethernet).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finish()
}

/// Versão incremental: permite calcular o checksum de vários pedaços
/// como se fossem um só.
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    pub fn new() -> Crc32 {
        Crc32 { state: 0xFFFF_FFFF }
    }

    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.state ^= byte as u32;
            for _ in 0..8 {
                // Se o bit mais baixo é 1, desloca e aplica o polinômio
                // (0xEDB88320 é o polinômio do CRC-32 com os bits invertidos).
                let mask = (self.state & 1).wrapping_neg();
                self.state = (self.state >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }

    pub fn finish(&self) -> u32 {
        !self.state
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Crc32::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valor_conhecido() {
        // Vetor de teste clássico do CRC-32.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn incremental_igual_ao_direto() {
        let mut c = Crc32::new();
        c.update(b"1234");
        c.update(b"56789");
        assert_eq!(c.finish(), crc32(b"123456789"));
    }
}
