//! Utilitários para testes: diretórios temporários que se apagam sozinhos.
//!
//! O módulo é público (e não `#[cfg(test)]`) porque os testes de integração
//! em `tests/` também o usam. Mas não faz parte do banco em si.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

pub struct TempDir {
    root: PathBuf,
}

impl TempDir {
    pub fn new(name: &str) -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("mate-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("criar diretório temporário");
        TempDir { root }
    }

    pub fn path(&self, file: &str) -> PathBuf {
        self.root.join(file)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
