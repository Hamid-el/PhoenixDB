use crate::paging::buffer_pool::BufferPoolManager;
use crate::paging::error::Result;
use crate::paging::replacement::ReplacementStrategy;
use crate::paging::types::PageId;
use crate::query::cursor::BTreeCursor;
use crate::query::{Operator, Record};

pub struct TableScan<'a, R: ReplacementStrategy, const VALUE_SIZE: usize> {
    cursor: BTreeCursor<'a, R, VALUE_SIZE>,
    decode_fn: fn(&[u8]) -> Record,
    is_open: bool,
}

impl<'a, R: ReplacementStrategy, const VALUE_SIZE: usize> TableScan<'a, R, VALUE_SIZE> {
    pub fn new(
        bpm: &'a BufferPoolManager<R>,
        root_page_id: PageId,
        decode_fn: fn(&[u8]) -> Record,
    ) -> Self {
        Self {
            cursor: BTreeCursor::new(bpm, root_page_id, 0, None),
            decode_fn,
            is_open: false,
        }
    }

    pub fn range(
        bpm: &'a BufferPoolManager<R>,
        root_page_id: PageId,
        start_key: u64,
        end_key: u64,
        decode_fn: fn(&[u8]) -> Record,
    ) -> Self {
        Self {
            cursor: BTreeCursor::new(bpm, root_page_id, start_key, Some(end_key)),
            decode_fn,
            is_open: false,
        }
    }
}

impl<R: ReplacementStrategy, const VALUE_SIZE: usize> Operator for TableScan<'_, R, VALUE_SIZE> {
    fn open(&mut self) -> Result<()> {
        self.cursor.open()?;
        self.is_open = true;
        Ok(())
    }

    fn next(&mut self) -> Result<Option<Record>> {
        match self.cursor.next()? {
            Some(raw) => Ok(Some((self.decode_fn)(&raw))),
            None => Ok(None),
        }
    }

    fn close(&mut self) -> Result<()> {
        self.is_open = false;
        self.cursor.close()
    }
}
