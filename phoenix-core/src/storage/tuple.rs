//! Row (tuple) serialization: turns a `Vec<Value>` into the fixed-size
//! byte array the B+Tree stores as its value, and back.
//!
//! Layout: `[column count: u8]` then per column a tag byte followed by the
//! payload — INT: 4 bytes LE; VARCHAR: u16 length + UTF-8 bytes;
//! BOOL: 1 byte. The remainder of the buffer is zero padding.

use crate::paging::error::{DbError, Result};
use crate::query::types::Value;

/// Fixed on-disk size for one row. Rows that serialize longer than this
/// are rejected at INSERT time.
pub const RECORD_SIZE: usize = 256;

const TAG_INT: u8 = 0;
const TAG_VARCHAR: u8 = 1;
const TAG_BOOL: u8 = 2;

pub fn encode_row(values: &[Value]) -> Result<[u8; RECORD_SIZE]> {
    if values.len() > u8::MAX as usize {
        return Err(DbError::Sql("too many columns in row".to_string()));
    }

    let mut buf = Vec::with_capacity(RECORD_SIZE);
    buf.push(values.len() as u8);

    for value in values {
        match value {
            Value::Int(n) => {
                buf.push(TAG_INT);
                buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::Varchar(s) => {
                if s.len() > u16::MAX as usize {
                    return Err(DbError::RecordTooLarge {
                        size: s.len(),
                        max: RECORD_SIZE,
                    });
                }
                buf.push(TAG_VARCHAR);
                buf.extend_from_slice(&(s.len() as u16).to_le_bytes());
                buf.extend_from_slice(s.as_bytes());
            }
            Value::Bool(b) => {
                buf.push(TAG_BOOL);
                buf.push(*b as u8);
            }
        }
    }

    if buf.len() > RECORD_SIZE {
        return Err(DbError::RecordTooLarge {
            size: buf.len(),
            max: RECORD_SIZE,
        });
    }

    let mut record = [0u8; RECORD_SIZE];
    record[..buf.len()].copy_from_slice(&buf);
    Ok(record)
}

pub fn decode_row(bytes: &[u8]) -> Result<Vec<Value>> {
    let corrupted = |reason: &str| DbError::Corrupted(format!("row decode: {}", reason));

    let mut pos = 0usize;
    let take = |pos: &mut usize, n: usize| -> Result<&[u8]> {
        if *pos + n > bytes.len() {
            return Err(corrupted("unexpected end of record"));
        }
        let slice = &bytes[*pos..*pos + n];
        *pos += n;
        Ok(slice)
    };

    let num_columns = take(&mut pos, 1)?[0] as usize;
    let mut values = Vec::with_capacity(num_columns);

    for _ in 0..num_columns {
        let tag = take(&mut pos, 1)?[0];
        let value = match tag {
            TAG_INT => {
                let raw = take(&mut pos, 4)?;
                Value::Int(i32::from_le_bytes(raw.try_into().unwrap()))
            }
            TAG_VARCHAR => {
                let raw = take(&mut pos, 2)?;
                let len = u16::from_le_bytes(raw.try_into().unwrap()) as usize;
                let raw = take(&mut pos, len)?;
                let s = std::str::from_utf8(raw)
                    .map_err(|_| corrupted("invalid UTF-8 in VARCHAR"))?;
                Value::Varchar(s.to_string())
            }
            TAG_BOOL => Value::Bool(take(&mut pos, 1)?[0] != 0),
            _ => return Err(corrupted("unknown value tag")),
        };
        values.push(value);
    }

    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let row = vec![
            Value::Int(42),
            Value::Varchar("hello world".to_string()),
            Value::Bool(true),
            Value::Int(-7),
        ];
        let encoded = encode_row(&row).unwrap();
        let decoded = decode_row(&encoded).unwrap();
        assert_eq!(row, decoded);
    }

    #[test]
    fn test_empty_row() {
        let encoded = encode_row(&[]).unwrap();
        assert_eq!(decode_row(&encoded).unwrap(), vec![]);
    }

    #[test]
    fn test_too_large_rejected() {
        let row = vec![Value::Varchar("x".repeat(RECORD_SIZE))];
        assert!(matches!(
            encode_row(&row),
            Err(DbError::RecordTooLarge { .. })
        ));
    }

    #[test]
    fn test_max_fitting_varchar() {
        // 1 (count) + 1 (tag) + 2 (len) leaves RECORD_SIZE - 4 bytes.
        let row = vec![Value::Varchar("y".repeat(RECORD_SIZE - 4))];
        let encoded = encode_row(&row).unwrap();
        assert_eq!(decode_row(&encoded).unwrap(), row);
    }
}
