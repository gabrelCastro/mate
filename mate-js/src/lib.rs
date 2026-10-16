//! Ponte entre o Mate (Rust) e o Node.js.
//!
//! O Node carrega bibliotecas nativas pela **N-API**, uma interface estável
//! em C para criar e ler valores JavaScript. O crate `napi` embrulha essa
//! interface em Rust. Com o atributo `#[napi]`, uma struct vira uma classe
//! JS e seus métodos viram métodos JS.
//!
//! Esta camada é deliberadamente fina. Ela só:
//! 1. converte valores JS → [`Value`] (os parâmetros `?`);
//! 2. chama o [`mate::Database`];
//! 3. converte o resultado → objetos JS;
//! 4. transforma [`DbError`] em exceção JS, com um código na frente da
//!    mensagem (`"CONSTRAINT: ..."`), que o `index.js` lê para montar um
//!    `MateError` com `.code`.
//!
//! A API amigável (`get`, `all`, `run`, `transaction`...) fica no
//! `index.js`, em JavaScript puro, porque lá é mais fácil de ler e mudar.

use mate::{Database, DbError, QueryResult, Value};
use napi::bindgen_prelude::*;
use napi::{Env, JsBigInt, JsObject, JsUnknown, ValueType};
use napi_derive::napi;

/// Maior inteiro que um `number` do JS representa sem perder precisão
/// (2^53 - 1). Inteiros maiores viram `BigInt`.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// O banco, visto do JavaScript. `None` depois de `close()`.
#[napi]
pub struct NativeDatabase {
    db: Option<Database>,
}

#[napi]
impl NativeDatabase {
    #[napi(constructor)]
    pub fn new(path: String) -> Result<Self> {
        let db = Database::open(&path).map_err(to_js_error)?;
        Ok(NativeDatabase { db: Some(db) })
    }

    /// Executa um ou mais comandos separados por `;`, sem parâmetros.
    #[napi]
    pub fn exec(&mut self, sql: String) -> Result<()> {
        self.db()?.execute(&sql).map_err(to_js_error)?;
        Ok(())
    }

    /// Executa um único comando com parâmetros `?` e devolve
    /// `{ columns, rows }` (consultas), `{ changes }` (INSERT/UPDATE/DELETE)
    /// ou `{ message }` (o resto).
    #[napi]
    pub fn query(&mut self, env: Env, sql: String, params: Vec<JsUnknown>) -> Result<JsObject> {
        let values = params.into_iter().map(js_to_value).collect::<Result<Vec<_>>>()?;
        let result = self.db()?.query_with_params(&sql, &values).map_err(to_js_error)?;
        result_to_js(&env, result)
    }

    #[napi(getter)]
    pub fn in_transaction(&self) -> bool {
        self.db.as_ref().is_some_and(|db| db.in_transaction())
    }

    /// Fecha o banco: faz o checkpoint e libera a trava do arquivo. Uma
    /// transação aberta é descartada.
    #[napi]
    pub fn close(&mut self) {
        self.db = None;
    }

    fn db(&mut self) -> Result<&mut Database> {
        self.db
            .as_mut()
            .ok_or_else(|| Error::new(Status::GenericFailure, "CLOSED: o banco já foi fechado"))
    }
}

// ----------------------------------------------------------------------
// JS → Rust
// ----------------------------------------------------------------------

fn js_to_value(value: JsUnknown) -> Result<Value> {
    Ok(match value.get_type()? {
        ValueType::Null | ValueType::Undefined => Value::Null,
        ValueType::Boolean => Value::Boolean(value.coerce_to_bool()?.get_value()?),
        ValueType::Number => {
            // JS só tem um tipo de número. Se o valor é inteiro (e cabe com
            // precisão), vira INTEGER. Senão, FLOAT. Uma coluna FLOAT aceita
            // INTEGER, então 2 numa coluna FLOAT funciona.
            let n = value.coerce_to_number()?.get_double()?;
            if n.fract() == 0.0 && n.abs() <= MAX_SAFE_INTEGER as f64 {
                Value::Integer(n as i64)
            } else {
                Value::Float(n)
            }
        }
        ValueType::BigInt => {
            // SAFETY: acabamos de conferir que o valor é um BigInt.
            let big: JsBigInt = unsafe { value.cast() };
            let (i, lossless) = big.get_i64()?;
            if !lossless {
                return Err(Error::new(
                    Status::InvalidArg,
                    "BigInt fora do intervalo de INTEGER (64 bits)",
                ));
            }
            Value::Integer(i)
        }
        ValueType::String => Value::Text(value.coerce_to_string()?.into_utf8()?.into_owned()?),
        other => {
            return Err(Error::new(
                Status::InvalidArg,
                format!(
                    "parâmetro de tipo não suportado: {other:?} (use number, bigint, string, boolean ou null)"
                ),
            ));
        }
    })
}

// ----------------------------------------------------------------------
// Rust → JS
// ----------------------------------------------------------------------

fn value_to_js(env: &Env, value: &Value) -> Result<JsUnknown> {
    Ok(match value {
        Value::Null => env.get_null()?.into_unknown(),
        Value::Integer(i) if i.unsigned_abs() <= MAX_SAFE_INTEGER => env.create_int64(*i)?.into_unknown(),
        Value::Integer(i) => env.create_bigint_from_i64(*i)?.into_unknown()?,
        Value::Float(x) => env.create_double(*x)?.into_unknown(),
        Value::Text(s) => env.create_string(s)?.into_unknown(),
        Value::Boolean(b) => env.get_boolean(*b)?.into_unknown(),
    })
}

fn result_to_js(env: &Env, result: QueryResult) -> Result<JsObject> {
    let mut obj = env.create_object()?;
    match result {
        QueryResult::Rows { columns, rows } => {
            let mut js_columns = env.create_array_with_length(columns.len())?;
            for (i, name) in columns.iter().enumerate() {
                js_columns.set_element(i as u32, env.create_string(name)?)?;
            }
            let mut js_rows = env.create_array_with_length(rows.len())?;
            for (i, row) in rows.iter().enumerate() {
                let mut js_row = env.create_array_with_length(row.len())?;
                for (j, value) in row.iter().enumerate() {
                    js_row.set_element(j as u32, value_to_js(env, value)?)?;
                }
                js_rows.set_element(i as u32, js_row)?;
            }
            obj.set_named_property("columns", js_columns)?;
            obj.set_named_property("rows", js_rows)?;
        }
        QueryResult::Affected { count, .. } => {
            obj.set_named_property("changes", env.create_double(count as f64)?)?;
        }
        QueryResult::Message(message) => {
            obj.set_named_property("message", env.create_string(&message)?)?;
        }
    }
    Ok(obj)
}

/// `DbError` → exceção JS. O código vai no começo da mensagem
/// (`"CONSTRAINT: valor duplicado..."`), e o `index.js` separa os dois.
fn to_js_error(err: DbError) -> Error {
    let code = match err {
        DbError::Io(_) => "IO",
        DbError::Corrupt(_) => "CORRUPT",
        DbError::Parse(_) => "PARSE",
        DbError::Plan(_) => "PLAN",
        DbError::Execution(_) => "EXECUTION",
        DbError::Constraint(_) => "CONSTRAINT",
        DbError::Transaction(_) => "TRANSACTION",
        DbError::Busy(_) => "BUSY",
    };
    Error::new(Status::GenericFailure, format!("{code}: {err}"))
}
