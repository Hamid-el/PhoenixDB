use crate::paging::constants::PAGE_SIZE;
use crate::paging::types::PageId;

const BITMAP_META_MAGIC: u8 = 0xB1;
const BITMAP_DATA_MAGIC: u8 = 0xB2;

// Metadata page layout (page 0 of the bitmap index file):
// [0]:       magic (0xB1)
// [1..5]:    num_values (u32 LE) - number of distinct values indexed
// [5..9]:    total_rows (u32 LE) - capacity of each bitset
// [9..13]:   first_directory_page (PageId)
// [13..PAGE_SIZE]: reserved

pub struct BitmapMetaPage<'a> {
    data: &'a [u8; PAGE_SIZE],
}

impl<'a> BitmapMetaPage<'a> {
    pub fn new(data: &'a [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn magic(&self) -> u8 {
        self.data[0]
    }

    pub fn is_valid(&self) -> bool {
        self.magic() == BITMAP_META_MAGIC
    }

    pub fn num_values(&self) -> u32 {
        u32::from_le_bytes(self.data[1..5].try_into().unwrap())
    }

    pub fn total_rows(&self) -> u32 {
        u32::from_le_bytes(self.data[5..9].try_into().unwrap())
    }

    pub fn first_directory_page(&self) -> PageId {
        u32::from_le_bytes(self.data[9..13].try_into().unwrap())
    }
}

pub struct BitmapMetaPageMut<'a> {
    data: &'a mut [u8; PAGE_SIZE],
}

impl<'a> BitmapMetaPageMut<'a> {
    pub fn new(data: &'a mut [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn init(&mut self, first_directory_page: PageId) {
        self.data.fill(0);
        self.data[0] = BITMAP_META_MAGIC;
        self.set_first_directory_page(first_directory_page);
    }

    pub fn set_num_values(&mut self, n: u32) {
        self.data[1..5].copy_from_slice(&n.to_le_bytes());
    }

    pub fn set_total_rows(&mut self, n: u32) {
        self.data[5..9].copy_from_slice(&n.to_le_bytes());
    }

    pub fn set_first_directory_page(&mut self, page_id: PageId) {
        self.data[9..13].copy_from_slice(&page_id.to_le_bytes());
    }
}

// Directory page layout:
// Maps value keys to their bitmap data page(s).
// [0]:       magic (0xB2)
// [1..3]:    num_entries (u16 LE)
// [3..7]:    next_directory_page (PageId, INVALID if none)
// [7..PAGE_SIZE]: entries array
//
// Each entry: key (u64 LE) + first_bitmap_page (PageId) + num_pages (u16 LE) = 14 bytes
// Max entries per page: (4096 - 7) / 14 = 292

const DIR_HEADER_SIZE: usize = 7;
const DIR_ENTRY_SIZE: usize = 14;
pub const DIR_MAX_ENTRIES: usize = (PAGE_SIZE - DIR_HEADER_SIZE) / DIR_ENTRY_SIZE;

#[derive(Clone, Copy, Debug)]
pub struct DirEntry {
    pub key: u64,
    pub first_bitmap_page: PageId,
    pub num_pages: u16,
}

pub struct BitmapDirPage<'a> {
    data: &'a [u8; PAGE_SIZE],
}

impl<'a> BitmapDirPage<'a> {
    pub fn new(data: &'a [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn num_entries(&self) -> u16 {
        u16::from_le_bytes(self.data[1..3].try_into().unwrap())
    }

    pub fn next_page(&self) -> PageId {
        u32::from_le_bytes(self.data[3..7].try_into().unwrap())
    }

    pub fn entry_at(&self, idx: usize) -> DirEntry {
        let offset = DIR_HEADER_SIZE + idx * DIR_ENTRY_SIZE;
        let key = u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap());
        let first_bitmap_page =
            u32::from_le_bytes(self.data[offset + 8..offset + 12].try_into().unwrap());
        let num_pages =
            u16::from_le_bytes(self.data[offset + 12..offset + 14].try_into().unwrap());
        DirEntry {
            key,
            first_bitmap_page,
            num_pages,
        }
    }

    pub fn find_key(&self, key: u64) -> Option<usize> {
        let n = self.num_entries() as usize;
        for i in 0..n {
            let entry = self.entry_at(i);
            if entry.key == key {
                return Some(i);
            }
        }
        None
    }
}

pub struct BitmapDirPageMut<'a> {
    data: &'a mut [u8; PAGE_SIZE],
}

impl<'a> BitmapDirPageMut<'a> {
    pub fn new(data: &'a mut [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn init(&mut self, next_page: PageId) {
        self.data.fill(0);
        self.data[0] = BITMAP_DATA_MAGIC;
        self.set_next_page(next_page);
    }

    pub fn num_entries(&self) -> u16 {
        u16::from_le_bytes(self.data[1..3].try_into().unwrap())
    }

    pub fn set_num_entries(&mut self, n: u16) {
        self.data[1..3].copy_from_slice(&n.to_le_bytes());
    }

    pub fn set_next_page(&mut self, page_id: PageId) {
        self.data[3..7].copy_from_slice(&page_id.to_le_bytes());
    }

    pub fn set_entry_at(&mut self, idx: usize, entry: &DirEntry) {
        let offset = DIR_HEADER_SIZE + idx * DIR_ENTRY_SIZE;
        self.data[offset..offset + 8].copy_from_slice(&entry.key.to_le_bytes());
        self.data[offset + 8..offset + 12]
            .copy_from_slice(&entry.first_bitmap_page.to_le_bytes());
        self.data[offset + 12..offset + 14]
            .copy_from_slice(&entry.num_pages.to_le_bytes());
    }

    pub fn append_entry(&mut self, entry: &DirEntry) -> bool {
        let n = self.num_entries() as usize;
        if n >= DIR_MAX_ENTRIES {
            return false;
        }
        self.set_entry_at(n, entry);
        self.set_num_entries((n + 1) as u16);
        true
    }
}

// Bitmap data page layout:
// Stores raw u64 words of a bitset. Each page holds up to 512 words (4096 bytes).
// For a single page: 512 * 64 = 32768 bits per page.
// Multi-page bitmaps use consecutive pages.

pub const WORDS_PER_PAGE: usize = PAGE_SIZE / 8;
pub const BITS_PER_PAGE: usize = WORDS_PER_PAGE * 64;

pub struct BitmapDataPage<'a> {
    data: &'a [u8; PAGE_SIZE],
}

impl<'a> BitmapDataPage<'a> {
    pub fn new(data: &'a [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn read_words(&self, out: &mut [u64]) {
        let count = out.len().min(WORDS_PER_PAGE);
        for i in 0..count {
            let offset = i * 8;
            out[i] = u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap());
        }
    }

    pub fn word_at(&self, idx: usize) -> u64 {
        let offset = idx * 8;
        u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap())
    }
}

pub struct BitmapDataPageMut<'a> {
    data: &'a mut [u8; PAGE_SIZE],
}

impl<'a> BitmapDataPageMut<'a> {
    pub fn new(data: &'a mut [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn clear(&mut self) {
        self.data.fill(0);
    }

    pub fn write_words(&mut self, words: &[u64]) {
        let count = words.len().min(WORDS_PER_PAGE);
        for i in 0..count {
            let offset = i * 8;
            self.data[offset..offset + 8].copy_from_slice(&words[i].to_le_bytes());
        }
    }

    pub fn set_bit(&mut self, bit_pos: usize) {
        let word_idx = bit_pos / 64;
        let bit_idx = bit_pos % 64;
        let offset = word_idx * 8;
        let mut word = u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap());
        word |= 1u64 << bit_idx;
        self.data[offset..offset + 8].copy_from_slice(&word.to_le_bytes());
    }

    pub fn clear_bit(&mut self, bit_pos: usize) {
        let word_idx = bit_pos / 64;
        let bit_idx = bit_pos % 64;
        let offset = word_idx * 8;
        let mut word = u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap());
        word &= !(1u64 << bit_idx);
        self.data[offset..offset + 8].copy_from_slice(&word.to_le_bytes());
    }
}

pub fn pages_needed_for_bits(num_bits: usize) -> usize {
    (num_bits + BITS_PER_PAGE - 1) / BITS_PER_PAGE
}
