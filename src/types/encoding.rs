//! Serialização: como valores e linhas viram bytes (e voltam).
//!
//! Páginas guardam bytes, não `Value`s. Este módulo define o formato:
//!
//! ```text
//!  Valor:  [tag: 1 byte][conteúdo]
//!
//!   tag 0  NULL      (sem conteúdo)
//!   tag 1  INTEGER   8 bytes, i64 little-endian
//!   tag 2  FLOAT     8 bytes, f64 little-endian
//!   tag 3  TEXT      4 bytes de tamanho (u32) + bytes UTF-8
//!   tag 4  BOOLEAN   1 byte (0 ou 1)
//!
//!  Linha:  [número de colunas: u16][valor 1][valor 2]...
//! ```
//!
//! Exemplo: a linha `(42, 'oi', NULL)` vira
//!
//! ```text
//!  03 00 | 01 2A 00 00 00 00 00 00 00 | 03 02 00 00 00 6F 69 | 00
//!  3 col.  INTEGER 42                   TEXT tamanho 2 "oi"     NULL
//! ```
//!
//! Como cada valor carrega a própria tag, dá para decodificar uma linha sem
//! conhecer o schema da tabela. Isso custa 1 byte por valor. Bancos como o
//! Postgres economizam esse byte e usam o schema (mais um bitmap de NULLs)
//! para decodificar.

use crate::error::{DbError, Result};
use crate::types::value::Value;

const TAG_NULL: u8 = 0;
const TAG_INTEGER: u8 = 1;
const TAG_FLOAT: u8 = 2;
const TAG_TEXT: u8 = 3;
const TAG_BOOLEAN: u8 = 4;

/// Acrescenta a representação de `value` ao final de `out`.
pub fn encode_value(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.push(TAG_NULL),
        Value::Integer(i) => {
            out.push(TAG_INTEGER);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Float(x) => {
            out.push(TAG_FLOAT);
            out.extend_from_slice(&x.to_le_bytes());
        }
        Value::Text(s) => {
            out.push(TAG_TEXT);
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        Value::Boolean(b) => {
            out.push(TAG_BOOLEAN);
            out.push(*b as u8);
        }
    }
}

/// Lê um valor a partir de `bytes[*pos..]` e avança `pos` para depois dele.
pub fn decode_value(bytes: &[u8], pos: &mut usize) -> Result<Value> {
    let tag = take(bytes, pos, 1)?[0];
    Ok(match tag {
        TAG_NULL => Value::Null,
        TAG_INTEGER => Value::Integer(i64::from_le_bytes(take(bytes, pos, 8)?.try_into().unwrap())),
        TAG_FLOAT => Value::Float(f64::from_le_bytes(take(bytes, pos, 8)?.try_into().unwrap())),
        TAG_TEXT => {
            let len = u32::from_le_bytes(take(bytes, pos, 4)?.try_into().unwrap()) as usize;
            let raw = take(bytes, pos, len)?;
            let s =
                std::str::from_utf8(raw).map_err(|_| DbError::Corrupt("texto com UTF-8 inválido".into()))?;
            Value::Text(s.to_string())
        }
        TAG_BOOLEAN => Value::Boolean(take(bytes, pos, 1)?[0] != 0),
        other => return Err(DbError::Corrupt(format!("tag de valor desconhecida: {other}"))),
    })
}

/// Serializa uma linha inteira.
pub fn encode_row(values: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(values.len() as u16).to_le_bytes());
    for v in values {
        encode_value(v, &mut out);
    }
    out
}

/// Desserializa uma linha produzida por [`encode_row`].
pub fn decode_row(bytes: &[u8]) -> Result<Vec<Value>> {
    let mut pos = 0;
    let count = u16::from_le_bytes(take(bytes, &mut pos, 2)?.try_into().unwrap());
    (0..count).map(|_| decode_value(bytes, &mut pos)).collect()
}

/// Pega `n` bytes a partir de `pos`, ou devolve erro se não houver bytes
/// suficientes (dado truncado ou corrompido).
fn take<'a>(bytes: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8]> {
    let end = *pos + n;
    if end > bytes.len() {
        return Err(DbError::Corrupt("registro truncado".into()));
    }
    let slice = &bytes[*pos..end];
    *pos = end;
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ida_e_volta() {
        let row = vec![
            Value::Integer(-7),
            Value::Float(3.25),
            Value::Text("olá, mundo".into()),
            Value::Boolean(true),
            Value::Null,
        ];
        let bytes = encode_row(&row);
        assert_eq!(decode_row(&bytes).unwrap(), row);
    }

    #[test]
    fn exemplo_da_documentacao() {
        let bytes = encode_row(&[Value::Integer(42), Value::Text("oi".into()), Value::Null]);
        assert_eq!(
            bytes,
            vec![3, 0, 1, 42, 0, 0, 0, 0, 0, 0, 0, 3, 2, 0, 0, 0, b'o', b'i', 0]
        );
    }

    #[test]
    fn bytes_truncados_dao_erro() {
        let bytes = encode_row(&[Value::Integer(1)]);
        assert!(decode_row(&bytes[..5]).is_err());
    }
}
