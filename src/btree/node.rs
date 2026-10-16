//! Os nós da B+Tree e como eles são gravados numa página.
//!
//! Cada nó ocupa exatamente uma página. Há dois tipos:
//!
//! ```text
//!   Nó interno: chaves separadoras e ponteiros para filhos
//!   ┌────┬─────┬────┬─────┬────┬─────┬────┐
//!   │ c0 │ k0  │ c1 │ k1  │ c2 │ k2  │ c3 │      n chaves, n+1 filhos
//!   └────┴─────┴────┴─────┴────┴─────┴────┘
//!     │          │          │          │
//!     ▼          ▼          ▼          ▼
//!   < k0     k0 ≤ x < k1  k1 ≤ x < k2  ≥ k2
//!
//!   Folha: as entradas de fato, mais um ponteiro para a próxima folha
//!   ┌──────────────────────────────────────┬──────┐
//!   │ (chave, rid) (chave, rid) ...         │ next │──► próxima folha
//!   └──────────────────────────────────────┴──────┘
//! ```
//!
//! Numa B+Tree (diferente da B-Tree "pura"), **só as folhas guardam
//! entradas**. Os nós internos guardam cópias de chaves que servem de placa
//! de sinalização. As folhas formam uma lista encadeada, então percorrer
//! um intervalo (`WHERE idade BETWEEN 20 AND 30`) é: achar a primeira folha
//! e seguir os ponteiros `next`.
//!
//! # Entradas: (chave, RecordId)
//!
//! Uma entrada é o par `(valor da coluna, endereço da linha)`. Ordenamos
//! pelo par inteiro, não só pela chave, e por isso **toda entrada é única**
//! mesmo num índice não-UNIQUE com valores repetidos. Assim a árvore nunca
//! precisa lidar com chaves duplicadas.
//!
//! # Formato em disco
//!
//! | offset | tamanho  | campo                                              |
//! |--------|----------|----------------------------------------------------|
//! | 0      | 1        | tipo: 1 = folha, 2 = interno                       |
//! | 1      | 2        | número de entradas (folha) ou de chaves (interno)  |
//! | 3      | 4        | folha: próxima folha (0 = nenhuma); interno: c0    |
//! | 7      | variável | folha: `[valor][rid]...`; interno: `[valor][rid][filho]...` |
//!
//! Para facilitar a leitura, os nós são **desserializados por inteiro** para
//! structs Rust, alterados e serializados de volta. Bancos de produção
//! manipulam os bytes da página diretamente, sem copiar, o que é mais
//! rápido e bem mais difícil de ler.

use crate::error::{DbError, Result};
use crate::heap::RecordId;
use crate::storage::page::{PAGE_SIZE, Page, PageId};
use crate::types::Value;
use crate::types::encoding::{decode_value, encode_value};

const LEAF: u8 = 1;
const INTERNAL: u8 = 2;

/// Uma entrada do índice. A ordem derivada compara primeiro `key`, depois
/// `rid`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexEntry {
    pub key: Value,
    pub rid: RecordId,
}

impl IndexEntry {
    /// Quantos bytes a entrada ocupa serializada (valor + RecordId).
    pub fn encoded_len(&self) -> usize {
        let mut buf = Vec::new();
        encode_entry(self, &mut buf);
        buf.len()
    }
}

#[derive(Debug, Clone)]
pub struct LeafNode {
    /// Entradas em ordem crescente.
    pub entries: Vec<IndexEntry>,
    pub next: Option<PageId>,
}

#[derive(Debug, Clone)]
pub struct InternalNode {
    /// `keys[i]` separa `children[i]` (entradas menores) de
    /// `children[i + 1]` (entradas maiores ou iguais).
    pub keys: Vec<IndexEntry>,
    pub children: Vec<PageId>,
}

#[derive(Debug, Clone)]
pub enum Node {
    Leaf(LeafNode),
    Internal(InternalNode),
}

impl Node {
    pub fn empty_leaf() -> Node {
        Node::Leaf(LeafNode {
            entries: Vec::new(),
            next: None,
        })
    }

    /// Serializa o nó (sem completar até o tamanho da página).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(PAGE_SIZE);
        match self {
            Node::Leaf(leaf) => {
                out.push(LEAF);
                out.extend_from_slice(&(leaf.entries.len() as u16).to_le_bytes());
                out.extend_from_slice(&leaf.next.unwrap_or(0).to_le_bytes());
                for e in &leaf.entries {
                    encode_entry(e, &mut out);
                }
            }
            Node::Internal(node) => {
                out.push(INTERNAL);
                out.extend_from_slice(&(node.keys.len() as u16).to_le_bytes());
                out.extend_from_slice(&node.children[0].to_le_bytes());
                for (key, child) in node.keys.iter().zip(&node.children[1..]) {
                    encode_entry(key, &mut out);
                    out.extend_from_slice(&child.to_le_bytes());
                }
            }
        }
        out
    }

    /// O nó cabe numa página? Se não couber, precisa ser dividido (*split*).
    pub fn fits_in_page(&self) -> bool {
        self.encode().len() <= PAGE_SIZE
    }

    pub fn to_page(&self) -> Page {
        let bytes = self.encode();
        assert!(
            bytes.len() <= PAGE_SIZE,
            "nó maior que a página: precisava de split"
        );
        let mut page = Page::new();
        page.bytes_mut()[..bytes.len()].copy_from_slice(&bytes);
        page
    }

    pub fn from_page(page: &Page) -> Result<Node> {
        let bytes = page.bytes();
        let count = page.read_u16(1) as usize;
        let header_ptr = page.read_u32(3);
        let mut pos = 7;
        match bytes[0] {
            LEAF => {
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    entries.push(decode_entry(bytes, &mut pos)?);
                }
                let next = if header_ptr == 0 { None } else { Some(header_ptr) };
                Ok(Node::Leaf(LeafNode { entries, next }))
            }
            INTERNAL => {
                let mut keys = Vec::with_capacity(count);
                let mut children = vec![header_ptr];
                for _ in 0..count {
                    keys.push(decode_entry(bytes, &mut pos)?);
                    children.push(read_u32(bytes, &mut pos));
                }
                Ok(Node::Internal(InternalNode { keys, children }))
            }
            other => Err(DbError::Corrupt(format!("tipo de nó B+Tree inválido: {other}"))),
        }
    }
}

fn encode_entry(e: &IndexEntry, out: &mut Vec<u8>) {
    encode_value(&e.key, out);
    out.extend_from_slice(&e.rid.page_id.to_le_bytes());
    out.extend_from_slice(&e.rid.slot.to_le_bytes());
}

fn decode_entry(bytes: &[u8], pos: &mut usize) -> Result<IndexEntry> {
    let key = decode_value(bytes, pos)?;
    let page_id = read_u32(bytes, pos);
    let slot = u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]);
    *pos += 2;
    Ok(IndexEntry {
        key,
        rid: RecordId { page_id, slot },
    })
}

fn read_u32(bytes: &[u8], pos: &mut usize) -> u32 {
    let v = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().unwrap());
    *pos += 4;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(k: i64, slot: u16) -> IndexEntry {
        IndexEntry {
            key: Value::Integer(k),
            rid: RecordId { page_id: 3, slot },
        }
    }

    #[test]
    fn folha_ida_e_volta() {
        let node = Node::Leaf(LeafNode {
            entries: vec![entry(1, 0), entry(2, 1)],
            next: Some(9),
        });
        let Node::Leaf(back) = Node::from_page(&node.to_page()).unwrap() else {
            panic!("esperava folha")
        };
        assert_eq!(back.entries, vec![entry(1, 0), entry(2, 1)]);
        assert_eq!(back.next, Some(9));
    }

    #[test]
    fn interno_ida_e_volta() {
        let node = Node::Internal(InternalNode {
            keys: vec![entry(10, 0), entry(20, 0)],
            children: vec![4, 5, 6],
        });
        let Node::Internal(back) = Node::from_page(&node.to_page()).unwrap() else {
            panic!("esperava nó interno")
        };
        assert_eq!(back.keys, vec![entry(10, 0), entry(20, 0)]);
        assert_eq!(back.children, vec![4, 5, 6]);
    }

    #[test]
    fn ordem_das_entradas() {
        // Mesma chave: desempata pelo RecordId.
        assert!(entry(5, 1) < entry(5, 2));
        assert!(entry(4, 9) < entry(5, 0));
    }
}
