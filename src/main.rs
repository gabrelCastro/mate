//! REPL: o terminal interativo do mate.
//!
//! ```text
//!   $ cargo run -- meu.db
//!   mate> CREATE TABLE t (id INT PRIMARY KEY, nome TEXT);
//!   mate> SELECT * FROM t;
//! ```
//!
//! Comandos SQL terminam com `;` e podem ocupar várias linhas. Comandos
//! começando com `.` são do próprio REPL (digite `.help`).
//!
//! Também aceita SQL pela entrada padrão:
//! `cargo run -- meu.db < examples/demo.sql`

use std::io::{self, BufRead, IsTerminal, Write};

use mate::Database;

const HELP: &str = "\
Comandos SQL terminam com ';'. Comandos especiais:
  .help             esta ajuda
  .tables           lista as tabelas
  .schema [tabela]  mostra o CREATE de cada tabela e índice
  .btree <índice>   desenha a B+Tree de um índice, nível por nível
  .stats            estatísticas do pager (cache, disco, WAL)
  .exit             sai (também: Ctrl+D)

Para ver o plano de uma consulta: EXPLAIN SELECT ...;
Para consultar o catálogo:        SELECT * FROM mate_schema;";

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "mate.db".to_string());
    let mut db = match Database::open(&path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("não foi possível abrir '{path}': {e}");
            std::process::exit(1);
        }
    };

    let interactive = io::stdin().is_terminal();
    if interactive {
        println!("Mate, banco '{path}'. Digite .help para ajuda.");
    }

    let mut buffer = String::new();
    let mut lines = io::stdin().lock().lines();
    loop {
        if interactive {
            let prompt = match (buffer.is_empty(), db.in_transaction()) {
                (true, false) => "mate> ",
                (true, true) => "mate*> ", // * = dentro de BEGIN
                (false, _) => "  ...> ",
            };
            print!("{prompt}");
            io::stdout().flush().ok();
        }

        let Some(Ok(line)) = lines.next() else {
            break; // fim da entrada (Ctrl+D)
        };

        // Comandos especiais só no começo de um comando novo.
        if buffer.is_empty() && line.trim_start().starts_with('.') {
            if !meta_command(&mut db, line.trim()) {
                break;
            }
            continue;
        }

        buffer.push_str(&line);
        buffer.push('\n');
        // Executa quando a linha termina com ';'.
        if !line.trim_end().ends_with(';') {
            continue;
        }
        let sql = std::mem::take(&mut buffer);
        match db.execute(&sql) {
            Ok(results) => {
                for result in results {
                    println!("{result}\n");
                }
            }
            Err(e) => eprintln!("ERRO: {e}\n"),
        }
    }

    if !buffer.trim().is_empty() {
        eprintln!("aviso: comando sem ';' no final foi ignorado");
    }
    if db.in_transaction() {
        eprintln!("aviso: transação aberta foi descartada (faltou COMMIT)");
    }
}

/// Executa um comando especial. Devolve `false` para sair do REPL.
fn meta_command(db: &mut Database, line: &str) -> bool {
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next();
    match cmd {
        ".exit" | ".quit" => return false,
        ".help" => println!("{HELP}"),
        ".tables" => {
            for table in db.catalog().tables() {
                println!("{}", table.name());
            }
        }
        ".schema" => {
            for table in db.catalog().tables() {
                if arg.is_some_and(|a| a != table.name()) {
                    continue;
                }
                let cols: Vec<String> = table
                    .schema
                    .columns
                    .iter()
                    .map(|c| format!("{} {}", c.name, c.data_type))
                    .collect();
                println!(
                    "{} ({})  -- heap na página {}",
                    table.name(),
                    cols.join(", "),
                    table.heap.first_page
                );
                for index in &table.indexes {
                    let unique = if index.unique { "UNIQUE " } else { "" };
                    println!(
                        "  {unique}INDEX {} ({})  -- raiz na página {}",
                        index.name, index.column, index.btree.root
                    );
                }
            }
        }
        ".btree" => match arg {
            None => eprintln!("uso: .btree <nome do índice>  (veja os nomes com .schema)"),
            Some(name) => match db.dump_index(name) {
                Ok(dump) => print!("{dump}"),
                Err(e) => eprintln!("ERRO: {e}"),
            },
        },
        ".stats" => {
            let s = db.stats();
            println!("páginas no banco:     {}", db.page_count());
            println!("acertos no cache:     {}", s.cache_hits);
            println!("faltas no cache:      {}", s.cache_misses);
            println!("leituras do disco:    {}", s.disk_reads);
            println!("escritas no disco:    {}", s.disk_writes);
            println!("quadros gravados WAL: {}", s.wal_frames_written);
            println!("checkpoints:          {}", s.checkpoints);
            println!("páginas recuperadas:  {}", s.recovered_pages);
        }
        other => eprintln!("comando desconhecido: {other} (digite .help)"),
    }
    true
}
