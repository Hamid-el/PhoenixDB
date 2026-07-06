use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Page {page_id} out of bounds (num_pages: {num_pages})")]
    PageOutOfBounds { page_id: u32, num_pages: u32 },

    #[error("Invalid page ID")]
    InvalidPageId,

    #[error("Page {page_id} is on the free list")]
    PageFreed { page_id: u32 },

    #[error("Page {page_id} already freed (double free)")]
    DoubleFree { page_id: u32 },

    #[error("Buffer pool full: all {pool_size} frames are pinned")]
    BufferPoolFull { pool_size: usize },

    #[error("Page {page_id} not found in buffer pool")]
    PageNotInPool { page_id: u32 },

    #[error("Duplicate key: {key}")]
    DuplicateKey { key: u64 },

    #[error("Key not found: {key}")]
    KeyNotFound { key: u64 },

    #[error("Corrupted node on page {page_id}: {reason}")]
    CorruptedNode { page_id: u32, reason: String },

    #[error("Corrupted metadata: {0}")]
    Corrupted(String),

    #[error("SQL error: {0}")]
    Sql(String),

    #[error("Table '{0}' does not exist")]
    TableNotFound(String),

    #[error("Table '{0}' already exists")]
    TableExists(String),

    #[error("Column '{0}' does not exist in table '{1}'")]
    ColumnNotFound(String, String),

    #[error("Type error: {0}")]
    TypeError(String),

    #[error("Record too large: {size} bytes (max {max})")]
    RecordTooLarge { size: usize, max: usize },

    #[error("Transaction error: {0}")]
    Transaction(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, DbError>;
