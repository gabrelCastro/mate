//! JOIN por laços aninhados (*nested loop join*).
//!
//! O algoritmo mais simples de JOIN:
//!
//! ```text
//!   para cada linha L da esquerda:
//!       para cada linha R da direita:
//!           se ON(L ++ R) é verdadeiro:
//!               devolve L ++ R
//! ```
//!
//! O custo é O(|esquerda| × |direita|). Para não reler a tabela da direita
//! do disco a cada linha da esquerda, ela é lida uma vez e guardada em
//! memória (materializada).
//!
//! Bancos reais têm alternativas bem mais rápidas:
//! - **Index nested loop**: se a coluna do ON na direita tem índice, busca
//!   por ele em vez de percorrer tudo.
//! - **Hash join**: monta uma tabela hash com a menor tabela e consulta com
//!   a maior. É O(n + m), mas só serve para igualdade.
//! - **Merge join**: com as duas entradas ordenadas pela chave, percorre
//!   ambas em paralelo.
//!
//! Implementar o hash join é um ótimo exercício (veja `docs/exercicios.md`).

use crate::error::Result;
use crate::execution::expr::BoundExpr;
use crate::execution::{Executor, Tuple};
use crate::sql::ast::JoinKind;
use crate::storage::pager::Pager;
use crate::types::Value;

pub struct NestedLoopJoin {
    left: Box<dyn Executor>,
    right: Box<dyn Executor>,
    kind: JoinKind,
    on: BoundExpr,
    right_width: usize,
    /// Linhas da direita, lidas na primeira chamada.
    right_rows: Option<Vec<Vec<Value>>>,
    /// A linha da esquerda sendo combinada no momento.
    current_left: Option<Vec<Value>>,
    /// Próxima linha da direita a testar contra `current_left`.
    right_pos: usize,
    /// `current_left` já teve algum par? (para o LEFT JOIN)
    matched: bool,
}

impl NestedLoopJoin {
    pub fn new(
        left: Box<dyn Executor>,
        right: Box<dyn Executor>,
        kind: JoinKind,
        on: BoundExpr,
        right_width: usize,
    ) -> NestedLoopJoin {
        NestedLoopJoin {
            left,
            right,
            kind,
            on,
            right_width,
            right_rows: None,
            current_left: None,
            right_pos: 0,
            matched: false,
        }
    }
}

impl Executor for NestedLoopJoin {
    fn next(&mut self, pager: &mut Pager) -> Result<Option<Tuple>> {
        if self.right_rows.is_none() {
            let mut rows = Vec::new();
            while let Some(t) = self.right.next(pager)? {
                rows.push(t.values);
            }
            self.right_rows = Some(rows);
        }
        let right_rows = self.right_rows.as_ref().unwrap();

        loop {
            // Precisa de uma nova linha da esquerda?
            let Some(left) = &self.current_left else {
                match self.left.next(pager)? {
                    None => return Ok(None),
                    Some(t) => {
                        self.current_left = Some(t.values);
                        self.right_pos = 0;
                        self.matched = false;
                        continue;
                    }
                }
            };

            // Laço interno: procura o próximo par.
            while self.right_pos < right_rows.len() {
                let right = &right_rows[self.right_pos];
                self.right_pos += 1;
                let combined: Vec<Value> = left.iter().chain(right).cloned().collect();
                if self.on.eval_condition(&combined)? {
                    self.matched = true;
                    return Ok(Some(Tuple::new(combined)));
                }
            }

            // Acabou a direita para esta linha da esquerda.
            let left = self.current_left.take().unwrap();
            if self.kind == JoinKind::Left && !self.matched {
                // LEFT JOIN sem par: devolve a esquerda com NULLs à direita.
                let mut row = left;
                row.extend(std::iter::repeat_n(Value::Null, self.right_width));
                return Ok(Some(Tuple::new(row)));
            }
        }
    }
}
