//! A B+Tree: busca, inserção (com divisão de nós), remoção e varredura.
//!
//! # Por que uma árvore?
//!
//! Sem índice, achar `WHERE id = 42` exige ler a tabela inteira: O(n)
//! páginas. A B+Tree acha a linha em O(log n) páginas. E, como cada nó é uma
//! página cheia de chaves (centenas por nó), a árvore é **muito** rasa. Com
//! 200 chaves por nó, 3 níveis já indexam 8 milhões de linhas.
//!
//! # Busca
//!
//! Começa na raiz. Em cada nó interno, escolhe o filho cujo intervalo
//! contém a chave e desce até chegar a uma folha.
//!
//! # Inserção e *split*
//!
//! A entrada é colocada na folha certa. Se a folha não couber mais na
//! página, ela se **divide** em duas: metade das entradas vai para uma
//! página nova, e a primeira entrada da metade direita sobe como separadora
//! para o pai. Se o pai também estourar, ele se divide, e assim por diante.
//!
//! ```text
//!   antes:            [ 10 20 30 40 ]   ← folha cheia, inserir 25
//!
//!   depois:      pai: [ ... 30 ... ]
//!                       ╱       ╲
//!          [ 10 20 25 ]          [ 30 40 ]
//! ```
//!
//! Se a **raiz** se divide, a árvore ganha um nível. Aqui usamos um truque
//! para a raiz nunca mudar de página: o conteúdo antigo da raiz é copiado
//! para uma página nova, e a página da raiz passa a ser um nó interno
//! apontando para as duas metades. Assim o catálogo, que guarda "o índice X
//! tem raiz na página N", nunca precisa ser atualizado.
//!
//! # Remoção (simplificada)
//!
//! A entrada é tirada da folha e nada mais. Numa B+Tree completa, nós que
//! ficam com menos da metade das entradas pegam emprestado do vizinho ou se
//! fundem com ele (*merge*). Muitos bancos reais também pulam essa etapa
//! (ou a adiam para manutenção) porque, na prática, os espaços vagos voltam
//! a ser preenchidos por inserções futuras. Implementar o merge fica como
//! exercício.

use std::ops::Bound;

use crate::btree::node::{IndexEntry, InternalNode, LeafNode, Node};
use crate::error::{DbError, Result};
use crate::storage::page::PageId;
use crate::storage::pager::Pager;
use crate::types::Value;
use crate::types::encoding::encode_value;

/// Tamanho máximo (serializado) de uma chave de índice. Garante que um nó
/// sempre tenha várias entradas e que a divisão em duas metades funcione.
pub const MAX_KEY_SIZE: usize = 1024;

#[derive(Debug, Clone, Copy)]
pub struct BTree {
    pub root: PageId,
}

/// Resultado de inserir num nó que precisou se dividir: a separadora que
/// deve subir para o pai e a página da nova metade direita.
type Split = Option<(IndexEntry, PageId)>;

impl BTree {
    /// Cria uma árvore vazia: uma raiz que é uma folha sem entradas.
    pub fn create(pager: &mut Pager) -> Result<BTree> {
        let root = pager.allocate()?;
        pager.write(root, Node::empty_leaf().to_page())?;
        Ok(BTree { root })
    }

    pub fn open(root: PageId) -> BTree {
        BTree { root }
    }

    // ------------------------------------------------------------------
    // Inserção
    // ------------------------------------------------------------------

    pub fn insert(&self, pager: &mut Pager, entry: IndexEntry) -> Result<()> {
        let mut key_bytes = Vec::new();
        encode_value(&entry.key, &mut key_bytes);
        if key_bytes.len() > MAX_KEY_SIZE {
            return Err(DbError::Execution(format!(
                "chave de índice com {} bytes excede o máximo de {MAX_KEY_SIZE}",
                key_bytes.len()
            )));
        }

        if let Some((separator, right)) = self.insert_into(pager, self.root, entry)? {
            // A raiz se dividiu. A metade esquerda está na própria página da
            // raiz e vai para uma página nova. A raiz vira um nó interno
            // com dois filhos.
            let left = pager.allocate()?;
            let left_content = pager.read(self.root)?.clone();
            pager.write(left, left_content)?;
            let new_root = Node::Internal(InternalNode {
                keys: vec![separator],
                children: vec![left, right],
            });
            pager.write(self.root, new_root.to_page())?;
        }
        Ok(())
    }

    /// Insere recursivamente na subárvore com raiz em `page_id`.
    fn insert_into(&self, pager: &mut Pager, page_id: PageId, entry: IndexEntry) -> Result<Split> {
        match Node::from_page(pager.read(page_id)?)? {
            Node::Leaf(mut leaf) => {
                // Busca binária pela posição da nova entrada.
                let pos = match leaf.entries.binary_search(&entry) {
                    Ok(_) => return Err(DbError::Corrupt("entrada duplicada no índice".into())),
                    Err(pos) => pos,
                };
                leaf.entries.insert(pos, entry);
                self.write_leaf_or_split(pager, page_id, leaf)
            }
            Node::Internal(mut node) => {
                let child_idx = child_index(&node, &entry);
                let Some((separator, new_child)) =
                    self.insert_into(pager, node.children[child_idx], entry)?
                else {
                    return Ok(None); // o filho não se dividiu: nada a fazer aqui
                };
                // O filho se dividiu: registramos a nova metade aqui.
                node.keys.insert(child_idx, separator);
                node.children.insert(child_idx + 1, new_child);
                self.write_internal_or_split(pager, page_id, node)
            }
        }
    }

    fn write_leaf_or_split(&self, pager: &mut Pager, page_id: PageId, mut leaf: LeafNode) -> Result<Split> {
        if Node::Leaf(leaf.clone()).fits_in_page() {
            pager.write(page_id, Node::Leaf(leaf).to_page())?;
            return Ok(None);
        }
        // Split: a metade direita vai para uma página nova, que entra na
        // lista encadeada de folhas logo depois desta.
        let right_id = pager.allocate()?;
        let mid = leaf.entries.len() / 2;
        let right = LeafNode {
            entries: leaf.entries.split_off(mid),
            next: leaf.next,
        };
        leaf.next = Some(right_id);
        // Numa folha, a separadora é uma **cópia** da primeira entrada da
        // direita. A entrada continua na folha.
        let separator = right.entries[0].clone();

        pager.write(page_id, Node::Leaf(leaf).to_page())?;
        pager.write(right_id, Node::Leaf(right).to_page())?;
        Ok(Some((separator, right_id)))
    }

    fn write_internal_or_split(
        &self,
        pager: &mut Pager,
        page_id: PageId,
        mut node: InternalNode,
    ) -> Result<Split> {
        if Node::Internal(node.clone()).fits_in_page() {
            pager.write(page_id, Node::Internal(node).to_page())?;
            return Ok(None);
        }
        // Split de nó interno: a chave do meio **sobe** para o pai e não
        // fica em nenhuma das metades (diferente da folha, que guarda cópia).
        //
        //   [ k0 k1 k2 k3 k4 ]   →   sobe k2,  esquerda [k0 k1],  direita [k3 k4]
        let right_id = pager.allocate()?;
        let mid = node.keys.len() / 2;
        let right_keys = node.keys.split_off(mid + 1);
        let separator = node.keys.pop().expect("mid existe");
        let right_children = node.children.split_off(mid + 1);
        let right = InternalNode {
            keys: right_keys,
            children: right_children,
        };

        pager.write(page_id, Node::Internal(node).to_page())?;
        pager.write(right_id, Node::Internal(right).to_page())?;
        Ok(Some((separator, right_id)))
    }

    // ------------------------------------------------------------------
    // Remoção
    // ------------------------------------------------------------------

    /// Remove uma entrada. Devolve `false` se ela não existia.
    pub fn delete(&self, pager: &mut Pager, entry: &IndexEntry) -> Result<bool> {
        let mut page_id = self.root;
        loop {
            match Node::from_page(pager.read(page_id)?)? {
                Node::Internal(node) => page_id = node.children[child_index(&node, entry)],
                Node::Leaf(mut leaf) => {
                    let Ok(pos) = leaf.entries.binary_search(entry) else {
                        return Ok(false);
                    };
                    leaf.entries.remove(pos);
                    pager.write(page_id, Node::Leaf(leaf).to_page())?;
                    return Ok(true);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Busca e varredura
    // ------------------------------------------------------------------

    /// Posiciona um cursor na primeira entrada que satisfaz o limite
    /// inferior. Quem chama segue com `cursor.next()` e para quando
    /// ultrapassar o limite superior.
    pub fn seek(&self, pager: &mut Pager, lower: Bound<&Value>) -> Result<BTreeCursor> {
        // "Esta entrada está antes do limite inferior?"
        let before_lower = |e: &IndexEntry| match lower {
            Bound::Unbounded => false,
            Bound::Included(v) => e.key < *v,
            Bound::Excluded(v) => e.key <= *v,
        };
        let mut page_id = self.root;
        loop {
            match Node::from_page(pager.read(page_id)?)? {
                Node::Internal(node) => {
                    // Desce no primeiro filho que pode conter entradas
                    // dentro do limite.
                    let idx = node.keys.partition_point(before_lower);
                    page_id = node.children[idx];
                }
                Node::Leaf(leaf) => {
                    let pos = leaf.entries.partition_point(before_lower);
                    return Ok(BTreeCursor { leaf, pos });
                }
            }
        }
    }

    /// Existe alguma entrada com esta chave? Usado para checar UNIQUE.
    pub fn contains_key(&self, pager: &mut Pager, key: &Value) -> Result<bool> {
        let mut cursor = self.seek(pager, Bound::Included(key))?;
        Ok(matches!(cursor.next(pager)?, Some(e) if e.key == *key))
    }

    /// Desenha a árvore em texto, nível por nível (comando `.btree` do REPL).
    pub fn dump(&self, pager: &mut Pager) -> Result<String> {
        let mut out = String::new();
        let mut level = vec![self.root];
        let mut depth = 0;
        while !level.is_empty() {
            out.push_str(&format!("nível {depth}:"));
            let mut next_level = Vec::new();
            for page_id in level {
                match Node::from_page(pager.read(page_id)?)? {
                    Node::Internal(node) => {
                        let keys: Vec<String> = node.keys.iter().map(|k| k.key.to_string()).collect();
                        out.push_str(&format!("  [pág {page_id}: {}]", keys.join(" | ")));
                        next_level.extend(node.children);
                    }
                    Node::Leaf(leaf) => {
                        let keys: Vec<String> = leaf.entries.iter().map(|e| e.key.to_string()).collect();
                        out.push_str(&format!("  (pág {page_id}: {})", keys.join(" ")));
                    }
                }
            }
            out.push('\n');
            level = next_level;
            depth += 1;
        }
        Ok(out)
    }
}

/// Em qual filho de `node` a entrada `entry` está (ou deveria estar)?
/// É o número de separadoras `<= entry`.
fn child_index(node: &InternalNode, entry: &IndexEntry) -> usize {
    node.keys.partition_point(|k| k <= entry)
}

/// Cursor sobre as folhas: guarda a folha atual (já desserializada) e a
/// posição dentro dela.
pub struct BTreeCursor {
    leaf: LeafNode,
    pos: usize,
}

impl BTreeCursor {
    pub fn next(&mut self, pager: &mut Pager) -> Result<Option<IndexEntry>> {
        loop {
            if let Some(entry) = self.leaf.entries.get(self.pos) {
                self.pos += 1;
                return Ok(Some(entry.clone()));
            }
            // Fim da folha: segue o ponteiro para a próxima. O loop também
            // pula folhas que ficaram vazias por remoções.
            let Some(next) = self.leaf.next else {
                return Ok(None);
            };
            match Node::from_page(pager.read(next)?)? {
                Node::Leaf(leaf) => self.leaf = leaf,
                Node::Internal(_) => {
                    return Err(DbError::Corrupt(
                        "ponteiro de folha aponta para nó interno".into(),
                    ));
                }
            }
            self.pos = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::RecordId;
    use crate::test_util::TempDir;

    fn entry(k: i64) -> IndexEntry {
        IndexEntry {
            key: Value::Integer(k),
            rid: RecordId {
                page_id: 1,
                slot: (k % 1000) as u16,
            },
        }
    }

    fn collect(tree: &BTree, pager: &mut Pager, lower: Bound<&Value>) -> Vec<i64> {
        let mut cursor = tree.seek(pager, lower).unwrap();
        let mut out = Vec::new();
        while let Some(e) = cursor.next(pager).unwrap() {
            let Value::Integer(k) = e.key else { panic!() };
            out.push(k);
        }
        out
    }

    #[test]
    fn insere_muitas_chaves_e_varre_em_ordem() {
        let dir = TempDir::new("btree");
        let mut pager = Pager::open(&dir.path("t.db")).unwrap();
        let tree = BTree::create(&mut pager).unwrap();

        // Ordem embaralhada (multiplicar por um primo módulo n permuta 0..n).
        let n = 5000;
        for i in 0..n {
            tree.insert(&mut pager, entry((i * 7919) % n)).unwrap();
        }
        // Cada folha comporta ~270 entradas inteiras: 5000 chaves exigem
        // vários splits e uma raiz interna (2 níveis).
        assert!(tree.dump(&mut pager).unwrap().contains("nível 1"));

        let all = collect(&tree, &mut pager, Bound::Unbounded);
        assert_eq!(all, (0..n).collect::<Vec<_>>());

        let from = collect(&tree, &mut pager, Bound::Included(&Value::Integer(4990)));
        assert_eq!(from, (4990..n).collect::<Vec<_>>());
        let after = collect(&tree, &mut pager, Bound::Excluded(&Value::Integer(4990)));
        assert_eq!(after, (4991..n).collect::<Vec<_>>());
    }

    #[test]
    fn chaves_repetidas_com_rids_diferentes() {
        let dir = TempDir::new("btree-dup");
        let mut pager = Pager::open(&dir.path("t.db")).unwrap();
        let tree = BTree::create(&mut pager).unwrap();
        for slot in 0..300u16 {
            let e = IndexEntry {
                key: Value::Text("mesmo valor".into()),
                rid: RecordId { page_id: 2, slot },
            };
            tree.insert(&mut pager, e).unwrap();
        }
        let key = Value::Text("mesmo valor".into());
        let mut cursor = tree.seek(&mut pager, Bound::Included(&key)).unwrap();
        let mut count = 0;
        while cursor.next(&mut pager).unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 300);
    }

    #[test]
    fn remove() {
        let dir = TempDir::new("btree-del");
        let mut pager = Pager::open(&dir.path("t.db")).unwrap();
        let tree = BTree::create(&mut pager).unwrap();
        for i in 0..2000 {
            tree.insert(&mut pager, entry(i)).unwrap();
        }
        for i in (0..2000).filter(|i| i % 3 != 0) {
            assert!(tree.delete(&mut pager, &entry(i)).unwrap());
        }
        assert!(!tree.delete(&mut pager, &entry(1)).unwrap());
        let left = collect(&tree, &mut pager, Bound::Unbounded);
        assert_eq!(left, (0..2000).filter(|i| i % 3 == 0).collect::<Vec<_>>());
        assert!(tree.contains_key(&mut pager, &Value::Integer(999)).unwrap());
        assert!(!tree.contains_key(&mut pager, &Value::Integer(1000)).unwrap());
    }
}
