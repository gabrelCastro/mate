//! Teste diferencial: o Mate e o SQLite devem dar as mesmas respostas.
//!
//! # A ideia
//!
//! Escrever à mão o resultado esperado de cada consulta não escala. O teste
//! diferencial troca a pergunta "o resultado está certo?" por "dois bancos
//! independentes concordam?":
//!
//! 1. cria as mesmas tabelas e dados no Mate e num SQLite em memória;
//! 2. gera milhares de consultas aleatórias (mas válidas);
//! 3. roda cada uma nos dois e compara os resultados;
//! 4. intercala INSERT/UPDATE/DELETE aleatórios e confere que as tabelas
//!    continuam idênticas.
//!
//! Se discordarem, um dos dois tem um bug. Quase sempre é o banco mais novo.
//! O próprio SQLite e o CockroachDB usam essa técnica. A ferramenta
//! SQLancer encontrou centenas de bugs em bancos famosos assim.
//!
//! # Gerando só o que os dois concordam
//!
//! Mate e SQLite discordam de propósito em alguns pontos, e o gerador evita
//! esses casos:
//! - o SQLite aceita comparar texto com número, e o Mate dá erro: o gerador
//!   só produz comparações entre valores do mesmo tipo;
//! - divisão por zero dá NULL no SQLite e erro no Mate: divisores são
//!   sempre constantes diferentes de zero;
//! - o LIKE do SQLite ignora maiúsculas: os textos são todos minúsculos;
//! - o SQLite não tem BOOLEAN: comparações não aparecem no SELECT;
//! - sem ORDER BY, cada banco devolve as linhas numa ordem: comparamos os
//!   resultados como multiconjuntos (ordenados). Com LIMIT, ordenamos por
//!   `id` (única) para o resultado ser determinístico.
//!
//! # Reproduzindo uma falha
//!
//! A falha mostra a semente e a consulta. Para repetir exatamente a mesma
//! sequência:
//!
//! ```text
//! MATE_DIFF_SEED=1234 cargo test --test diferencial
//! ```
//!
//! `MATE_DIFF_CASES=20000` aumenta o número de consultas por semente.

use mate::test_util::TempDir;
use mate::{Database, Value};
use rusqlite::types::ValueRef;

const SCHEMA: &str = "
    CREATE TABLE t1 (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, s TEXT);
    CREATE TABLE t2 (id INTEGER PRIMARY KEY, t1_id INTEGER, c INTEGER, s TEXT);
    CREATE INDEX t1_a ON t1 (a);
    CREATE INDEX t1_s ON t1 (s);
    CREATE INDEX t2_t1 ON t2 (t1_id);
";

const WORDS: &[&str] = &["ana", "bia", "caio", "davi", "eva", "fabio"];
const LIKE_PATTERNS: &[&str] = &["a%", "%a", "%i%", "_a%", "%", "d_v_", "e%a"];

#[test]
fn mate_concorda_com_sqlite() {
    let seeds: Vec<u64> = match std::env::var("MATE_DIFF_SEED") {
        Ok(s) => vec![s.parse().expect("MATE_DIFF_SEED deve ser um número")],
        Err(_) => vec![1, 2, 3, 4],
    };
    let cases: usize = std::env::var("MATE_DIFF_CASES")
        .ok()
        .map(|s| s.parse().expect("MATE_DIFF_CASES deve ser um número"))
        .unwrap_or(400);
    for seed in seeds {
        run_seed(seed, cases);
    }
}

fn run_seed(seed: u64, cases: usize) {
    let dir = TempDir::new("diferencial");
    let mut pair = Pair {
        mate: Database::open(dir.path("diff.db")).unwrap(),
        sqlite: rusqlite::Connection::open_in_memory().unwrap(),
        seed,
    };
    let mut generator = Generator::new(seed);

    pair.execute(SCHEMA);
    for sql in generator.initial_data() {
        pair.execute(&sql);
    }

    for case in 0..cases {
        if case % 8 == 7 {
            let sql = generator.modification();
            pair.compare_modification(&sql);
            pair.compare_query("SELECT * FROM t1");
            pair.compare_query("SELECT * FROM t2");
        } else {
            let sql = generator.query();
            pair.compare_query(&sql);
        }
    }
}

// ----------------------------------------------------------------------
// Rodando nos dois bancos e comparando
// ----------------------------------------------------------------------

struct Pair {
    mate: Database,
    sqlite: rusqlite::Connection,
    seed: u64,
}

/// Um resultado normalizado para comparação: cada linha como lista de
/// textos, e as linhas ordenadas.
type Rows = Vec<Vec<String>>;

impl Pair {
    /// Executa nos dois sem comparar (preparação).
    fn execute(&mut self, sql: &str) {
        self.mate
            .execute(sql)
            .unwrap_or_else(|e| panic!("Mate falhou na preparação: {e}\n{sql}"));
        self.sqlite
            .execute_batch(sql)
            .unwrap_or_else(|e| panic!("SQLite falhou na preparação: {e}\n{sql}"));
    }

    fn compare_query(&mut self, sql: &str) {
        let mate = self.mate.query(sql).map(|r| {
            let rows = r
                .rows()
                .iter()
                .map(|row| row.iter().map(mate_cell).collect())
                .collect();
            sorted(rows)
        });
        let sqlite = sqlite_query(&self.sqlite, sql).map(sorted);
        match (mate, sqlite) {
            (Ok(m), Ok(s)) if m == s => {}
            (Ok(m), Ok(s)) => self.fail(sql, &format!("Mate:\n{}\nSQLite:\n{}", show(&m), show(&s))),
            (Err(e), Ok(_)) => self.fail(sql, &format!("o Mate deu erro e o SQLite não: {e}")),
            (Ok(_), Err(e)) => self.fail(sql, &format!("o SQLite deu erro e o Mate não: {e}")),
            (Err(_), Err(_)) => {} // os dois recusaram: concordam
        }
    }

    fn compare_modification(&mut self, sql: &str) {
        let mate = self.mate.query(sql);
        let sqlite = self.sqlite.execute(sql, []);
        match (&mate, &sqlite) {
            (Ok(_), Ok(_)) | (Err(_), Err(_)) => {}
            _ => self.fail(sql, &format!("Mate: {mate:?}\nSQLite: {sqlite:?}")),
        }
    }

    fn fail(&self, sql: &str, details: &str) -> ! {
        panic!(
            "\nMate e SQLite discordam (semente {}).\nSQL: {sql}\n{details}\n\
             Para repetir: MATE_DIFF_SEED={} cargo test --test diferencial\n",
            self.seed, self.seed
        );
    }
}

fn sqlite_query(conn: &rusqlite::Connection, sql: &str) -> rusqlite::Result<Rows> {
    let mut stmt = conn.prepare(sql)?;
    let n = stmt.column_count();
    let rows = stmt.query_map([], |row| {
        (0..n).map(|i| row.get_ref(i).map(sqlite_cell)).collect()
    })?;
    rows.collect()
}

/// Formas canônicas de cada valor. Floats são arredondados para tolerar
/// diferenças mínimas de arredondamento (no AVG, por exemplo).
fn mate_cell(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Integer(i) => format!("int {i}"),
        Value::Float(x) => format!("real {x:.6}"),
        Value::Text(s) => format!("text {s}"),
        Value::Boolean(b) => format!("int {}", *b as i64),
    }
}

fn sqlite_cell(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Null => "NULL".into(),
        ValueRef::Integer(i) => format!("int {i}"),
        ValueRef::Real(x) => format!("real {x:.6}"),
        ValueRef::Text(t) => format!("text {}", String::from_utf8_lossy(t)),
        ValueRef::Blob(_) => panic!("o gerador nunca produz BLOB"),
    }
}

fn sorted(mut rows: Rows) -> Rows {
    rows.sort();
    rows
}

fn show(rows: &Rows) -> String {
    let mut out: Vec<String> = rows
        .iter()
        .take(15)
        .map(|r| format!("  {}", r.join(" | ")))
        .collect();
    if rows.len() > 15 {
        out.push(format!("  ... ({} linhas no total)", rows.len()));
    }
    if rows.is_empty() {
        out.push("  (nenhuma linha)".into());
    }
    out.join("\n")
}

// ----------------------------------------------------------------------
// Gerador de SQL aleatório
// ----------------------------------------------------------------------

/// Gerador pseudoaleatório xorshift64*: poucas linhas, determinístico a
/// partir da semente, e suficiente para testes (não serve para criptografia).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Número em `0..n`.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// Verdadeiro com probabilidade `percent`%.
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }

    fn int_between(&mut self, low: i64, high: i64) -> i64 {
        low + self.below((high - low + 1) as u64) as i64
    }
}

/// As colunas visíveis numa consulta, separadas por tipo.
struct Columns {
    ints: Vec<&'static str>,
    texts: Vec<&'static str>,
}

impl Columns {
    fn t1() -> Columns {
        Columns {
            ints: vec!["id", "a", "b"],
            texts: vec!["s"],
        }
    }

    fn join() -> Columns {
        Columns {
            ints: vec!["t1.id", "t1.a", "t1.b", "t2.id", "t2.t1_id", "t2.c"],
            texts: vec!["t1.s", "t2.s"],
        }
    }
}

struct Generator {
    rng: Rng,
    next_t1_id: i64,
    next_t2_id: i64,
}

impl Generator {
    fn new(seed: u64) -> Generator {
        Generator {
            // xorshift não pode começar em zero.
            rng: Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1),
            next_t1_id: 1,
            next_t2_id: 1,
        }
    }

    fn initial_data(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..6 {
            out.push(self.insert_t1(10));
        }
        for _ in 0..8 {
            out.push(self.insert_t2(15));
        }
        out
    }

    // -- valores ----------------------------------------------------------

    fn int_literal(&mut self) -> String {
        self.rng.int_between(-3, 12).to_string()
    }

    fn int_or_null(&mut self) -> String {
        if self.rng.chance(12) {
            "NULL".into()
        } else {
            self.int_literal()
        }
    }

    fn text_literal(&mut self) -> String {
        format!("'{}'", self.rng.pick(WORDS))
    }

    fn text_or_null(&mut self) -> String {
        if self.rng.chance(12) {
            "NULL".into()
        } else {
            self.text_literal()
        }
    }

    // -- expressões ---------------------------------------------------------

    fn int_expr(&mut self, cols: &Columns, depth: u32) -> String {
        let choice = if depth == 0 {
            self.rng.below(2)
        } else {
            self.rng.below(8)
        };
        match choice {
            0 => self.rng.pick(&cols.ints).to_string(),
            1 => self.int_literal(),
            2 | 3 => {
                let op = self.rng.pick(&["+", "-", "*"]);
                let l = self.int_expr(cols, depth - 1);
                let r = self.int_expr(cols, depth - 1);
                format!("({l} {op} {r})")
            }
            4 => {
                // Divisor constante e diferente de zero (veja o topo do arquivo).
                let op = self.rng.pick(&["/", "%"]);
                let divisor = self.rng.int_between(1, 4);
                format!("({} {op} {divisor})", self.int_expr(cols, depth - 1))
            }
            5 => format!("ABS({})", self.int_expr(cols, depth - 1)),
            6 => format!("COALESCE({}, {})", self.rng.pick(&cols.ints), self.int_literal()),
            _ => format!("LENGTH({})", self.text_expr(cols, depth - 1)),
        }
    }

    fn text_expr(&mut self, cols: &Columns, depth: u32) -> String {
        let choice = if depth == 0 {
            self.rng.below(2)
        } else {
            self.rng.below(5)
        };
        match choice {
            0 => self.rng.pick(&cols.texts).to_string(),
            1 => self.text_literal(),
            2 => format!("UPPER({})", self.text_expr(cols, depth - 1)),
            3 => format!("LOWER({})", self.text_expr(cols, depth - 1)),
            _ => {
                let l = self.text_expr(cols, depth - 1);
                let r = self.text_expr(cols, depth - 1);
                format!("({l} || {r})")
            }
        }
    }

    fn predicate(&mut self, cols: &Columns, depth: u32) -> String {
        let choice = if depth == 0 {
            self.rng.below(7)
        } else {
            self.rng.below(10)
        };
        let cmp = ["=", "!=", "<", "<=", ">", ">="];
        match choice {
            0 | 1 => {
                // Coluna comparada com constante: o caso que usa índices.
                let col = self.rng.pick(&cols.ints).to_string();
                let op = self.rng.pick(&cmp);
                let lit = self.int_literal();
                if self.rng.chance(50) {
                    format!("{col} {op} {lit}")
                } else {
                    format!("{lit} {op} {col}")
                }
            }
            2 => {
                let op = self.rng.pick(&cmp);
                let l = self.int_expr(cols, 1);
                let r = self.int_expr(cols, 1);
                format!("{l} {op} {r}")
            }
            3 => {
                let op = self.rng.pick(&cmp);
                let col = self.rng.pick(&cols.texts).to_string();
                format!("{col} {op} {}", self.text_literal())
            }
            4 => {
                let col = self.rng.pick(&cols.ints).to_string();
                let not = if self.rng.chance(50) { "NOT " } else { "" };
                format!("{col} IS {not}NULL")
            }
            5 => {
                let col = self.rng.pick(&cols.ints).to_string();
                let items: Vec<String> = (0..self.rng.int_between(1, 4))
                    .map(|_| self.int_or_null())
                    .collect();
                let not = if self.rng.chance(30) { "NOT " } else { "" };
                format!("{col} {not}IN ({})", items.join(", "))
            }
            6 => {
                let col = self.rng.pick(&cols.texts).to_string();
                let not = if self.rng.chance(30) { "NOT " } else { "" };
                format!("{col} {not}LIKE '{}'", self.rng.pick(LIKE_PATTERNS))
            }
            7 => format!("NOT ({})", self.predicate(cols, depth - 1)),
            _ => {
                let op = self.rng.pick(&["AND", "OR"]);
                let l = self.predicate(cols, depth - 1);
                let r = self.predicate(cols, depth - 1);
                format!("({l} {op} {r})")
            }
        }
    }

    fn maybe_where(&mut self, cols: &Columns) -> String {
        if self.rng.chance(75) {
            format!(" WHERE {}", self.predicate(cols, 2))
        } else {
            String::new()
        }
    }

    fn projection(&mut self, cols: &Columns) -> String {
        let n = self.rng.int_between(1, 4);
        let items: Vec<String> = (0..n)
            .map(|_| {
                if self.rng.chance(70) {
                    self.int_expr(cols, 2)
                } else {
                    self.text_expr(cols, 2)
                }
            })
            .collect();
        items.join(", ")
    }

    // -- consultas ----------------------------------------------------------

    fn query(&mut self) -> String {
        match self.rng.below(6) {
            0 | 1 => self.simple_select(),
            2 => self.aggregate_select(),
            3 => self.join_select(),
            4 => self.range_select(),
            _ => self.distinct_select(),
        }
    }

    fn simple_select(&mut self) -> String {
        let cols = Columns::t1();
        let proj = if self.rng.chance(15) {
            "*".to_string()
        } else {
            self.projection(&cols)
        };
        let filter = self.maybe_where(&cols);
        let mut sql = format!("SELECT {proj} FROM t1{filter}");
        if self.rng.chance(30) {
            let limit = self.rng.int_between(0, 8);
            let offset = self.rng.int_between(0, 5);
            sql.push_str(&format!(" ORDER BY id LIMIT {limit} OFFSET {offset}"));
        }
        sql
    }

    /// Busca focada em índices: intervalo numa coluna indexada.
    fn range_select(&mut self) -> String {
        let col = self.rng.pick(&["id", "a"]);
        let low = self.int_literal();
        let high = self.int_literal();
        let extra = if self.rng.chance(40) {
            format!(" AND {}", self.predicate(&Columns::t1(), 1))
        } else {
            String::new()
        };
        match self.rng.below(3) {
            0 => format!("SELECT id, a, s FROM t1 WHERE {col} = {low}{extra}"),
            1 => format!("SELECT id, a, s FROM t1 WHERE {col} BETWEEN {low} AND {high}{extra}"),
            _ => format!("SELECT id, a, s FROM t1 WHERE s = {}{extra}", self.text_literal()),
        }
    }

    fn aggregate_select(&mut self) -> String {
        let cols = Columns::t1();
        let x = self.int_expr(&cols, 1);
        let aggs = format!("COUNT(*), COUNT({x}), SUM({x}), MIN({x}), MAX({x}), AVG({x})");
        let filter = self.maybe_where(&cols);
        if self.rng.chance(25) {
            return format!("SELECT {aggs} FROM t1{filter}");
        }
        let group = self
            .rng
            .pick(&["a", "b", "s", "(a % 3)", "LENGTH(s)"])
            .to_string();
        let having = if self.rng.chance(40) {
            format!(" HAVING COUNT(*) > {}", self.rng.int_between(0, 3))
        } else {
            String::new()
        };
        format!("SELECT {group}, {aggs} FROM t1{filter} GROUP BY {group}{having}")
    }

    fn join_select(&mut self) -> String {
        let cols = Columns::join();
        let kind = if self.rng.chance(50) { "JOIN" } else { "LEFT JOIN" };
        let proj = self.projection(&cols);
        let filter = self.maybe_where(&cols);
        format!("SELECT t1.id, t2.id, {proj} FROM t1 {kind} t2 ON t2.t1_id = t1.id{filter}")
    }

    fn distinct_select(&mut self) -> String {
        let cols = Columns::t1();
        let proj = self.projection(&cols);
        let filter = self.maybe_where(&cols);
        format!("SELECT DISTINCT {proj} FROM t1{filter}")
    }

    // -- modificações -------------------------------------------------------

    fn modification(&mut self) -> String {
        match self.rng.below(10) {
            0..=2 => {
                let n = self.rng.int_between(1, 4);
                self.insert_t1(n)
            }
            3 | 4 => {
                let n = self.rng.int_between(1, 4);
                self.insert_t2(n)
            }
            5..=7 => {
                // Nunca altera `id`: veja o topo do arquivo.
                let cols = Columns::t1();
                let a = self.int_expr(&cols, 1);
                // LOWER mantém os dados minúsculos (o LIKE do SQLite ignora
                // maiúsculas).
                let s = format!("LOWER({})", self.text_expr(&cols, 1));
                let set = match self.rng.below(3) {
                    0 => format!("a = {a}"),
                    1 => format!("s = {s}"),
                    _ => format!("a = {a}, b = a, s = {s}"),
                };
                format!("UPDATE t1 SET {set} WHERE {}", self.predicate(&cols, 1))
            }
            8 => {
                let cols = Columns {
                    ints: vec!["id", "t1_id", "c"],
                    texts: vec!["s"],
                };
                format!("DELETE FROM t2 WHERE {}", self.predicate(&cols, 1))
            }
            _ => {
                // Às vezes repete um id: os dois devem recusar (PRIMARY KEY).
                let id = self.rng.int_between(1, self.next_t1_id);
                format!("INSERT INTO t1 VALUES ({id}, 1, 1, 'ana')")
            }
        }
    }

    fn insert_t1(&mut self, rows: i64) -> String {
        let values: Vec<String> = (0..rows)
            .map(|_| {
                let id = self.next_t1_id;
                self.next_t1_id += 1;
                format!(
                    "({id}, {}, {}, {})",
                    self.int_or_null(),
                    self.int_or_null(),
                    self.text_or_null()
                )
            })
            .collect();
        format!("INSERT INTO t1 VALUES {}", values.join(", "))
    }

    fn insert_t2(&mut self, rows: i64) -> String {
        let values: Vec<String> = (0..rows)
            .map(|_| {
                let id = self.next_t2_id;
                self.next_t2_id += 1;
                let t1_id = if self.rng.chance(10) {
                    "NULL".to_string()
                } else {
                    self.rng.int_between(1, self.next_t1_id + 3).to_string()
                };
                format!("({id}, {t1_id}, {}, {})", self.int_or_null(), self.text_or_null())
            })
            .collect();
        format!("INSERT INTO t2 VALUES {}", values.join(", "))
    }
}
