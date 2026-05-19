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

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, DbError>;
