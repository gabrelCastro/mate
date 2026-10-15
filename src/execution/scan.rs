//! Os executores-folha: de onde as linhas saem.

use std::ops::Bound;

use crate::btree::{BTree, BTreeCursor};
use crate::catalog::TableInfo;
use crate::error::{DbError, Result};
use crate::execution::{Executor, Tuple};
use crate::heap::{HeapCursor, TableHeap};
use crate::storage::pager::Pager;
use crate::types::Value;
use crate::types::encoding::decode_row;

/// Devolve uma única linha sem colunas, para `SELECT 1 + 1`.
pub struct SingleRow {
    done: bool,
}

impl SingleRow {
    pub fn new() -> SingleRow {
        SingleRow { done: false }
    }
}

impl Default for SingleRow {
    fn default() -> Self {
        SingleRow::new()
    }
}

impl Executor for SingleRow {
    fn next(&mut self, _pager: &mut Pager) -> Result<Option<Tuple>> {
        if self.done {
            return Ok(None);
        }
        self.done = true;
        Ok(Some(Tuple::new(vec![])))
    }
}

/// Varredura sequencial: todas as linhas do heap, na ordem física.
pub struct SeqScan {
    cursor: HeapCursor,
}

impl SeqScan {
    pub fn new(table: &TableInfo) -> SeqScan {
        SeqScan {
            cursor: table.heap.scan(),
        }
    }
}

impl Executor for SeqScan {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        let Some((rid, bytes)) = self.cursor.next(pager)? else {
            return Ok(None);
        };
        Ok(Some(Tuple {
            rid: Some(rid),
            values: decode_row(&bytes)?,
        }))
    }
}

/// Varredura por índice: percorre as entradas da B+Tree dentro do intervalo
/// e, para cada uma, busca a linha no heap pelo `RecordId`.
///
/// ```text
///   B+Tree (idade)                 heap
///   ┌────────────────────┐
///   │ 18 → (pág 4, slot 2)│ ─────► linha de Ana
///   │ 21 → (pág 2, slot 0)│ ─────► linha de Bruno
///   │ 25 → (pág 4, slot 7)│ ─────► linha de Carla
///   └────────────────────┘
/// ```
///
/// Cada entrada custa uma leitura de página aleatória no heap. Por isso,
/// quando o intervalo cobre boa parte da tabela, um SeqScan sai mais barato.
/// Otimizadores reais usam estatísticas para decidir. O nosso sempre
/// prefere o índice.
pub struct IndexScan {
    heap: TableHeap,
    btree: BTree,
    lower: Bound<Value>,
    upper: Bound<Value>,
    /// Criado no primeiro `next` (o seek precisa do pager).
    cursor: Option<BTreeCursor>,
}

impl IndexScan {
    pub fn new(
        table: &TableInfo,
        index_name: &str,
        lower: Bound<Value>,
        upper: Bound<Value>,
    ) -> Result<IndexScan> {
        let index = table
            .indexes
            .iter()
            .find(|i| i.name == index_name)
            .ok_or_else(|| DbError::Plan(format!("o índice '{index_name}' não existe")))?;
        Ok(IndexScan {
            heap: table.heap,
            btree: index.btree,
            lower,
            upper,
            cursor: None,
        })
    }

    fn beyond_upper(&self, key: &Value) -> bool {
        match &self.upper {
            Bound::Unbounded => false,
            Bound::Included(v) => key > v,
            Bound::Excluded(v) => key >= v,
        }
    }
}

impl Executor for IndexScan {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        if self.cursor.is_none() {
            self.cursor = Some(self.btree.seek(pager, self.lower.as_ref())?);
        }
        let cursor = self.cursor.as_mut().unwrap();
        loop {
            let Some(entry) = cursor.next(pager)? else {
                return Ok(None);
            };
            // NULLs ficam no começo do índice (são os menores valores); uma
            // comparação nunca é verdadeira para eles, então os pulamos.
            if entry.key.is_null() {
                continue;
            }
            if self.beyond_upper(&entry.key) {
                return Ok(None); // as entradas estão em ordem: acabou
            }
            let Some(bytes) = self.heap.get(pager, entry.rid)? else {
                return Err(DbError::Corrupt(format!(
                    "índice aponta para linha inexistente {}",
                    entry.rid
                )));
            };
            return Ok(Some(Tuple {
                rid: Some(entry.rid),
                values: decode_row(&bytes)?,
            }));
        }
    }
}
