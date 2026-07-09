use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::encoding::{ByteReader, ByteWriter};
use crate::paging::error::{DbError, Result};
use crate::paging::types::PageId;
use crate::sql::ast::DataType;

const CATALOG_MAGIC: &[u8; 4] = b"PHXC";
const CATALOG_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub data_type: DataType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexDef {
    pub name: String,
    pub column: String,
    pub root_page_id: PageId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<Column>,
    pub root_page_id: PageId,
    pub next_row_id: u64,
    pub indexes: Vec<IndexDef>,
}

impl TableSchema {
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.column_index(name).map(|i| &self.columns[i])
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    tables: BTreeMap<String, TableSchema>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads the catalog from disk, or returns an empty catalog if the file
    /// does not exist yet (fresh database).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::new());
        }
        let bytes = fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// Saves the catalog atomically: write a temp file, then rename it over
    /// the real file.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let tmp_path = path.with_extension("catalog.tmp");

        let bytes = self.to_bytes();
        {
            use std::io::Write;
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    pub fn get_table(&self, name: &str) -> Option<&TableSchema> {
        self.tables.get(&name.to_ascii_lowercase())
    }

    pub fn get_table_mut(&mut self, name: &str) -> Option<&mut TableSchema> {
        self.tables.get_mut(&name.to_ascii_lowercase())
    }

    pub fn add_table(&mut self, schema: TableSchema) -> Result<()> {
        let key = schema.name.to_ascii_lowercase();
        if self.tables.contains_key(&key) {
            return Err(DbError::TableExists(schema.name));
        }
        self.tables.insert(key, schema);
        Ok(())
    }

    pub fn tables(&self) -> impl Iterator<Item = &TableSchema> {
        self.tables.values()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = ByteWriter::new();
        w.put_bytes(CATALOG_MAGIC);
        w.put_u8(CATALOG_VERSION);
        w.put_u32(self.tables.len() as u32);

        for table in self.tables.values() {
            w.put_str(&table.name);
            w.put_u32(table.root_page_id);
            w.put_u64(table.next_row_id);

            w.put_u16(table.columns.len() as u16);
            for column in &table.columns {
                w.put_str(&column.name);
                w.put_u8(match column.data_type {
                    DataType::Int => 0,
                    DataType::Varchar => 1,
                });
            }

            w.put_u16(table.indexes.len() as u16);
            for index in &table.indexes {
                w.put_str(&index.name);
                w.put_str(&index.column);
                w.put_u32(index.root_page_id);
            }
        }

        w.into_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut r = ByteReader::new(bytes);

        let magic = r.get_bytes(4)?;
        if magic != CATALOG_MAGIC {
            return Err(DbError::Corrupted("bad catalog magic".to_string()));
        }
        let version = r.get_u8()?;
        if version != CATALOG_VERSION {
            return Err(DbError::Corrupted(format!(
                "unsupported catalog version {}",
                version
            )));
        }

        let num_tables = r.get_u32()?;
        let mut tables = BTreeMap::new();

        for _ in 0..num_tables {
            let name = r.get_str()?;
            let root_page_id = r.get_u32()?;
            let next_row_id = r.get_u64()?;

            let num_columns = r.get_u16()?;
            let mut columns = Vec::with_capacity(num_columns as usize);
            for _ in 0..num_columns {
                let col_name = r.get_str()?;
                let data_type = match r.get_u8()? {
                    0 => DataType::Int,
                    1 => DataType::Varchar,
                    other => {
                        return Err(DbError::Corrupted(format!(
                            "unknown column type tag {}",
                            other
                        )));
                    }
                };
                columns.push(Column {
                    name: col_name,
                    data_type,
                });
            }

            let num_indexes = r.get_u16()?;
            let mut indexes = Vec::with_capacity(num_indexes as usize);
            for _ in 0..num_indexes {
                indexes.push(IndexDef {
                    name: r.get_str()?,
                    column: r.get_str()?,
                    root_page_id: r.get_u32()?,
                });
            }

            let key = name.to_ascii_lowercase();
            tables.insert(
                key,
                TableSchema {
                    name,
                    columns,
                    root_page_id,
                    next_row_id,
                    indexes,
                },
            );
        }

        Ok(Self { tables })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_catalog() -> Catalog {
        let mut catalog = Catalog::new();
        catalog
            .add_table(TableSchema {
                name: "users".to_string(),
                columns: vec![
                    Column {
                        name: "id".to_string(),
                        data_type: DataType::Int,
                    },
                    Column {
                        name: "name".to_string(),
                        data_type: DataType::Varchar,
                    },
                ],
                root_page_id: 3,
                next_row_id: 17,
                indexes: vec![IndexDef {
                    name: "users_id_idx".to_string(),
                    column: "id".to_string(),
                    root_page_id: 9,
                }],
            })
            .unwrap();
        catalog
    }

    #[test]
    fn test_roundtrip_bytes() {
        let catalog = sample_catalog();
        let bytes = catalog.to_bytes();
        let loaded = Catalog::from_bytes(&bytes).unwrap();
        assert_eq!(catalog, loaded);
    }

    #[test]
    fn test_save_and_load_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.catalog");

        let catalog = sample_catalog();
        catalog.save(&path).unwrap();

        let loaded = Catalog::load(&path).unwrap();
        assert_eq!(catalog, loaded);

        let users = loaded.get_table("USERS").unwrap();
        assert_eq!(users.name, "users");
        assert_eq!(users.root_page_id, 3);
        assert_eq!(users.next_row_id, 17);
        assert_eq!(users.columns.len(), 2);
        assert_eq!(users.indexes.len(), 1);
    }

    #[test]
    fn test_load_missing_file_is_empty() {
        let dir = tempdir().unwrap();
        let catalog = Catalog::load(dir.path().join("nope.catalog")).unwrap();
        assert_eq!(catalog.tables().count(), 0);
    }

    #[test]
    fn test_duplicate_table_rejected() {
        let mut catalog = sample_catalog();
        let result = catalog.add_table(TableSchema {
            name: "USERS".to_string(),
            columns: vec![],
            root_page_id: 0,
            next_row_id: 1,
            indexes: vec![],
        });
        assert!(matches!(result, Err(DbError::TableExists(_))));
    }

    #[test]
    fn test_corrupted_magic_rejected() {
        let result = Catalog::from_bytes(b"NOPE\x01\x00\x00\x00\x00");
        assert!(matches!(result, Err(DbError::Corrupted(_))));
    }
}
