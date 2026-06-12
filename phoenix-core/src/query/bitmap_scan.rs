use crate::paging::buffer_pool::BufferPoolManager;
use crate::paging::error::Result;
use crate::paging::replacement::ReplacementStrategy;
use crate::paging::types::PageId;
use crate::query::cursor::BTreeCursor;
use crate::query::{Operator, Record};
use crate::storage::bitmap::BitmapIndex;
use crate::storage::bitmap_set::BitSet;

pub struct BitmapScan<'a, R: ReplacementStrategy, const VALUE_SIZE: usize> {
    bpm: &'a BufferPoolManager<R>,
    table_root: PageId,
    bitmap: Option<BitSet>,
    cursor: Option<BTreeCursor<'a, R, VALUE_SIZE>>,
    decode_fn: fn(&[u8]) -> Record,
    positions: Vec<usize>,
    pos_idx: usize,
    is_open: bool,
}

impl<'a, R: ReplacementStrategy, const VALUE_SIZE: usize> BitmapScan<'a, R, VALUE_SIZE> {
    pub fn new(
        bpm: &'a BufferPoolManager<R>,
        table_root: PageId,
        bitmap: BitSet,
        decode_fn: fn(&[u8]) -> Record,
    ) -> Self {
        Self {
            bpm,
            table_root,
            bitmap: Some(bitmap),
            cursor: None,
            decode_fn,
            positions: Vec::new(),
            pos_idx: 0,
            is_open: false,
        }
    }

    pub fn from_index(
        bpm: &'a BufferPoolManager<R>,
        table_root: PageId,
        index_meta_page: PageId,
        key: u64,
        decode_fn: fn(&[u8]) -> Record,
    ) -> Result<Self> {
        let idx = BitmapIndex::open(bpm, index_meta_page);
        let bitmap = idx.lookup(key)?;
        Ok(Self::new(bpm, table_root, bitmap, decode_fn))
    }

    pub fn with_and(
        bpm: &'a BufferPoolManager<R>,
        table_root: PageId,
        index_meta_page: PageId,
        keys: &[u64],
        decode_fn: fn(&[u8]) -> Record,
    ) -> Result<Self> {
        let idx = BitmapIndex::open(bpm, index_meta_page);
        let mut result: Option<BitSet> = None;
        for &k in keys {
            let bs = idx.lookup(k)?;
            result = Some(match result {
                Some(acc) => acc.and(&bs),
                None => bs,
            });
        }
        let bitmap = result.unwrap_or_else(|| BitSet::new(0));
        Ok(Self::new(bpm, table_root, bitmap, decode_fn))
    }

    pub fn with_or(
        bpm: &'a BufferPoolManager<R>,
        table_root: PageId,
        index_meta_page: PageId,
        keys: &[u64],
        decode_fn: fn(&[u8]) -> Record,
    ) -> Result<Self> {
        let idx = BitmapIndex::open(bpm, index_meta_page);
        let mut result: Option<BitSet> = None;
        for &k in keys {
            let bs = idx.lookup(k)?;
            result = Some(match result {
                Some(acc) => acc.or(&bs),
                None => bs,
            });
        }
        let bitmap = result.unwrap_or_else(|| BitSet::new(0));
        Ok(Self::new(bpm, table_root, bitmap, decode_fn))
    }
}


impl<R: ReplacementStrategy, const VALUE_SIZE: usize> Operator for BitmapScan<'_, R, VALUE_SIZE> {
    fn open(&mut self) -> Result<()> {
        if let Some(ref bitmap) = self.bitmap {
            self.positions = bitmap.iter_ones().collect();
        }
        self.pos_idx = 0;
        self.cursor = Some(BTreeCursor::new(self.bpm, self.table_root, 0, None));
        self.cursor.as_mut().unwrap().open()?;
        self.is_open = true;
        Ok(())
    }

    fn next(&mut self) -> Result<Option<Record>> {
        while self.pos_idx < self.positions.len() {
            let target_row = self.positions[self.pos_idx] as u64;
            self.pos_idx += 1;

            let cursor = self.cursor.as_mut().unwrap();
            cursor.close()?;
            *cursor = BTreeCursor::new(self.bpm, self.table_root, target_row, Some(target_row));
            cursor.open()?;

            if let Some(raw) = cursor.next()? {
                return Ok(Some((self.decode_fn)(&raw)));
            }
        }
        Ok(None)
    }

    fn close(&mut self) -> Result<()> {
        if let Some(ref mut cursor) = self.cursor {
            cursor.close()?;
        }
        self.is_open = false;
        Ok(())
    }
}
