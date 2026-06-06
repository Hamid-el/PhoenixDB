use crate::paging::buffer_pool::BufferPoolManager;
use crate::paging::constants::INVALID_PAGE_ID;
use crate::paging::error::{DbError, Result};
use crate::paging::replacement::ReplacementStrategy;
use crate::paging::types::PageId;
use crate::storage::bitmap_page::{
    BitmapDataPage, BitmapDataPageMut, BitmapDirPage, BitmapDirPageMut, BitmapMetaPage,
    BitmapMetaPageMut, DirEntry, BITS_PER_PAGE, WORDS_PER_PAGE, pages_needed_for_bits,
};
use crate::storage::bitmap_set::BitSet;

pub struct BitmapIndex<'a, R: ReplacementStrategy> {
    bpm: &'a BufferPoolManager<R>,
    meta_page_id: PageId,
}

impl<'a, R: ReplacementStrategy> BitmapIndex<'a, R> {
    pub fn create(bpm: &'a BufferPoolManager<R>) -> Result<Self> {
        let meta_handle = bpm.new_page()?;
        let meta_page_id = meta_handle.page_id();

        let dir_handle = bpm.new_page()?;
        let dir_page_id = dir_handle.page_id();

        {
            let mut dir_data = dir_handle.write();
            let mut dir = BitmapDirPageMut::new(&mut dir_data);
            dir.init(INVALID_PAGE_ID);
        }
        bpm.unpin_page(dir_page_id, true)?;

        {
            let mut meta_data = meta_handle.write();
            let mut meta = BitmapMetaPageMut::new(&mut meta_data);
            meta.init(dir_page_id);
        }
        bpm.unpin_page(meta_page_id, true)?;

        Ok(Self { bpm, meta_page_id })
    }

    pub fn open(bpm: &'a BufferPoolManager<R>, meta_page_id: PageId) -> Self {
        Self { bpm, meta_page_id }
    }

    pub fn meta_page_id(&self) -> PageId {
        self.meta_page_id
    }

    pub fn insert(&self, key: u64, row_id: u32) -> Result<()> {
        let total_rows = self.read_total_rows()?;
        let new_total = if row_id >= total_rows {
            row_id + 1
        } else {
            total_rows
        };

        if new_total > total_rows {
            self.update_total_rows(new_total)?;
        }

        match self.find_dir_entry(key)? {
            Some((dir_page_id, entry_idx, entry)) => {
                self.set_bit_in_bitmap(entry.first_bitmap_page, row_id as usize)?;
                if new_total > total_rows {
                    let old_pages = pages_needed_for_bits(total_rows as usize);
                    let new_pages = pages_needed_for_bits(new_total as usize);
                    if new_pages > old_pages {
                        self.extend_bitmap(dir_page_id, entry_idx, &entry, new_pages)?;
                    }
                }
                Ok(())
            }
            None => {
                let num_pages = pages_needed_for_bits(new_total as usize).max(1);
                let first_page = self.allocate_bitmap_pages(num_pages)?;
                self.set_bit_in_bitmap(first_page, row_id as usize)?;

                let new_entry = DirEntry {
                    key,
                    first_bitmap_page: first_page,
                    num_pages: num_pages as u16,
                };
                self.append_dir_entry(&new_entry)?;
                self.increment_num_values()?;
                Ok(())
            }
        }
    }

    pub fn lookup(&self, key: u64) -> Result<BitSet> {
        let total_rows = self.read_total_rows()?;
        match self.find_dir_entry(key)? {
            Some((_, _, entry)) => {
                self.read_bitmap(entry.first_bitmap_page, entry.num_pages, total_rows as usize)
            }
            None => Ok(BitSet::new(total_rows as usize)),
        }
    }

    pub fn total_rows(&self) -> Result<u32> {
        self.read_total_rows()
    }

    pub fn num_values(&self) -> Result<u32> {
        let handle = self.bpm.fetch_page(self.meta_page_id)?;
        let data = handle.read();
        let meta = BitmapMetaPage::new(&data);
        let n = meta.num_values();
        drop(data);
        self.bpm.unpin_page(self.meta_page_id, false)?;
        Ok(n)
    }

    fn read_total_rows(&self) -> Result<u32> {
        let handle = self.bpm.fetch_page(self.meta_page_id)?;
        let data = handle.read();
        let meta = BitmapMetaPage::new(&data);
        let total = meta.total_rows();
        drop(data);
        self.bpm.unpin_page(self.meta_page_id, false)?;
        Ok(total)
    }

    fn update_total_rows(&self, total: u32) -> Result<()> {
        let handle = self.bpm.fetch_page(self.meta_page_id)?;
        {
            let mut data = handle.write();
            let mut meta = BitmapMetaPageMut::new(&mut data);
            meta.set_total_rows(total);
        }
        self.bpm.unpin_page(self.meta_page_id, true)?;
        Ok(())
    }

    fn increment_num_values(&self) -> Result<()> {
        let handle = self.bpm.fetch_page(self.meta_page_id)?;
        let current = {
            let data = handle.read();
            let meta = BitmapMetaPage::new(&data);
            meta.num_values()
        };
        {
            let mut data = handle.write();
            let mut meta = BitmapMetaPageMut::new(&mut data);
            meta.set_num_values(current + 1);
        }
        self.bpm.unpin_page(self.meta_page_id, true)?;
        Ok(())
    }

    fn first_dir_page_id(&self) -> Result<PageId> {
        let handle = self.bpm.fetch_page(self.meta_page_id)?;
        let data = handle.read();
        let meta = BitmapMetaPage::new(&data);
        let dir_id = meta.first_directory_page();
        drop(data);
        self.bpm.unpin_page(self.meta_page_id, false)?;
        Ok(dir_id)
    }

    fn find_dir_entry(&self, key: u64) -> Result<Option<(PageId, usize, DirEntry)>> {
        let mut dir_page_id = self.first_dir_page_id()?;

        while dir_page_id != INVALID_PAGE_ID {
            let handle = self.bpm.fetch_page(dir_page_id)?;
            let data = handle.read();
            let dir = BitmapDirPage::new(&data);

            if let Some(idx) = dir.find_key(key) {
                let entry = dir.entry_at(idx);
                drop(data);
                self.bpm.unpin_page(dir_page_id, false)?;
                return Ok(Some((dir_page_id, idx, entry)));
            }

            let next = dir.next_page();
            drop(data);
            self.bpm.unpin_page(dir_page_id, false)?;
            dir_page_id = next;
        }

        Ok(None)
    }

    fn append_dir_entry(&self, entry: &DirEntry) -> Result<()> {
        let mut dir_page_id = self.first_dir_page_id()?;

        while dir_page_id != INVALID_PAGE_ID {
            let handle = self.bpm.fetch_page(dir_page_id)?;
            {
                let mut data = handle.write();
                let mut dir = BitmapDirPageMut::new(&mut data);
                if dir.append_entry(entry) {
                    drop(data);
                    self.bpm.unpin_page(dir_page_id, true)?;
                    return Ok(());
                }
            }
            let next = {
                let data = handle.read();
                let dir = BitmapDirPage::new(&data);
                dir.next_page()
            };
            self.bpm.unpin_page(dir_page_id, true)?;

            if next == INVALID_PAGE_ID {
                let new_dir_handle = self.bpm.new_page()?;
                let new_dir_id = new_dir_handle.page_id();
                {
                    let mut new_data = new_dir_handle.write();
                    let mut new_dir = BitmapDirPageMut::new(&mut new_data);
                    new_dir.init(INVALID_PAGE_ID);
                    new_dir.append_entry(entry);
                }
                self.bpm.unpin_page(new_dir_id, true)?;

                let handle = self.bpm.fetch_page(dir_page_id)?;
                {
                    let mut data = handle.write();
                    let mut dir = BitmapDirPageMut::new(&mut data);
                    dir.set_next_page(new_dir_id);
                }
                self.bpm.unpin_page(dir_page_id, true)?;
                return Ok(());
            }

            dir_page_id = next;
        }

        Err(DbError::Internal("no directory page found".into()))
    }

    fn allocate_bitmap_pages(&self, num_pages: usize) -> Result<PageId> {
        let mut first_page_id = INVALID_PAGE_ID;
        for i in 0..num_pages {
            let handle = self.bpm.new_page()?;
            let page_id = handle.page_id();
            if i == 0 {
                first_page_id = page_id;
            }
            {
                let mut data = handle.write();
                let mut bmp = BitmapDataPageMut::new(&mut data);
                bmp.clear();
            }
            self.bpm.unpin_page(page_id, true)?;
        }
        Ok(first_page_id)
    }

    fn set_bit_in_bitmap(&self, first_page: PageId, bit_pos: usize) -> Result<()> {
        let page_offset = bit_pos / BITS_PER_PAGE;
        let bit_in_page = bit_pos % BITS_PER_PAGE;
        let target_page = first_page + page_offset as u32;

        let handle = self.bpm.fetch_page(target_page)?;
        {
            let mut data = handle.write();
            let mut bmp = BitmapDataPageMut::new(&mut data);
            bmp.set_bit(bit_in_page);
        }
        self.bpm.unpin_page(target_page, true)?;
        Ok(())
    }

    fn extend_bitmap(
        &self,
        dir_page_id: PageId,
        entry_idx: usize,
        entry: &DirEntry,
        new_num_pages: usize,
    ) -> Result<()> {
        let old_num = entry.num_pages as usize;
        for _ in old_num..new_num_pages {
            let handle = self.bpm.new_page()?;
            let page_id = handle.page_id();
            {
                let mut data = handle.write();
                let mut bmp = BitmapDataPageMut::new(&mut data);
                bmp.clear();
            }
            self.bpm.unpin_page(page_id, true)?;
        }

        let handle = self.bpm.fetch_page(dir_page_id)?;
        {
            let mut data = handle.write();
            let mut dir = BitmapDirPageMut::new(&mut data);
            let updated = DirEntry {
                key: entry.key,
                first_bitmap_page: entry.first_bitmap_page,
                num_pages: new_num_pages as u16,
            };
            dir.set_entry_at(entry_idx, &updated);
        }
        self.bpm.unpin_page(dir_page_id, true)?;
        Ok(())
    }

    fn read_bitmap(&self, first_page: PageId, num_pages: u16, total_bits: usize) -> Result<BitSet> {
        let total_words = (total_bits + 63) / 64;
        let mut words = vec![0u64; total_words];
        let mut words_read = 0;

        for p in 0..num_pages as u32 {
            let page_id = first_page + p;
            let handle = self.bpm.fetch_page(page_id)?;
            let data = handle.read();
            let bmp = BitmapDataPage::new(&data);

            let words_this_page = WORDS_PER_PAGE.min(total_words - words_read);
            let mut page_words = vec![0u64; words_this_page];
            bmp.read_words(&mut page_words);
            words[words_read..words_read + words_this_page]
                .copy_from_slice(&page_words);

            drop(data);
            self.bpm.unpin_page(page_id, false)?;
            words_read += words_this_page;

            if words_read >= total_words {
                break;
            }
        }

        Ok(BitSet::from_raw_words(words, total_bits))
    }
}
