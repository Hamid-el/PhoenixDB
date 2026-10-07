use core::prelude::rust_2024::{Err, Ok};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use log::{debug, info};

use crate::paging::constants::{INVALID_PAGE_ID, PAGE_SIZE};
use crate::paging::error::{DbError, Result};
use crate::paging::types::PageId;

pub struct DiskManager {
    db_path: PathBuf,
    file: Mutex<File>,
    num_pages: AtomicU32,
    free_list: Mutex<VecDeque<PageId>>,
}

impl DiskManager {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let db_path = path.as_ref().to_path_buf();

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&db_path)?;

        let file_len = file.metadata()?.len();
        let num_pages = (file_len / PAGE_SIZE as u64) as u32;
        
        info!("DiskManager opened '{}' with {} pages",db_path.display(),num_pages);

        Ok(Self {
            db_path,
            file: Mutex::new(file),
            num_pages: AtomicU32::new(num_pages),
            free_list: Mutex::new(VecDeque::new()),
        })
    }

    pub fn read_page(&self, page_id: PageId) -> Result<[u8; PAGE_SIZE]> {
        self.validate_page_id(page_id)?;
        self.check_not_free(page_id)?;

        let offset = Self::page_offset(page_id);
        let mut buf = [0u8; PAGE_SIZE];

        let mut file = self.file
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut buf)?;

        debug!("Read page {}", page_id);
        Ok(buf)
    }

    pub fn write_page(&self, page_id: PageId, data: &[u8; PAGE_SIZE]) -> Result<()> {
        self.validate_page_id(page_id)?;
        self.check_not_free(page_id)?;

        let offset = Self::page_offset(page_id);

        let mut file = self.file
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;
        file.flush()?;

        debug!("Wrote page {}", page_id);
        Ok(())
    }

    pub fn allocate_page(&self) -> Result<PageId> {
        // Check free list first (reuse freed pages)
        {
            let mut free_list = self.free_list
                .lock()
                .map_err(|e| DbError::Internal(e.to_string()))?;
            if let Some(page_id) = free_list.pop_front() {
                info!("Allocated page {} (reused from free list)", page_id);
                return Ok(page_id);
            }
        }

        // No free pages — extend the file
        let new_page_id = self.num_pages.fetch_add(1, Ordering::SeqCst);
        let offset = Self::page_offset(new_page_id);
        let zeroed = [0u8; PAGE_SIZE];

        let mut file = self
            .file
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&zeroed)?;
        file.flush()?;

        info!("Allocated page {} (new)", new_page_id);
        Ok(new_page_id)
    }

    pub fn free_page(&self, page_id: PageId) -> Result<()> {
        if page_id == INVALID_PAGE_ID {
            return Err(DbError::InvalidPageId);
        }

        let current_pages = self.num_pages.load(Ordering::SeqCst);
        if page_id >= current_pages {
            return Err(DbError::PageOutOfBounds {
                page_id,
                num_pages: current_pages,
            });
        }

        let mut free_list = self
            .free_list
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;

        if free_list.contains(&page_id) {
            return Err(DbError::DoubleFree { page_id });
        }

        free_list.push_back(page_id);
        debug!("Freed page {}", page_id);
        Ok(())
    }

    /// Writes a page during WAL recovery, bypassing the bounds and free-list
    /// checks: the page may lie beyond the current end of the file (the
    /// crash happened after the WAL record was written but before the file
    /// was extended). Extends the file and the page counter as needed.
    pub fn write_page_raw(&self, page_id: PageId, data: &[u8; PAGE_SIZE]) -> Result<()> {
        if page_id == INVALID_PAGE_ID {
            return Err(DbError::InvalidPageId);
        }

        let offset = Self::page_offset(page_id);
        let mut file = self.file
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;
        file.flush()?;

        self.num_pages.fetch_max(page_id + 1, Ordering::SeqCst);
        debug!("Raw-wrote page {} (recovery)", page_id);
        Ok(())
    }

    /// Forces all written data down to the storage device (fsync).
    pub fn sync(&self) -> Result<()> {
        let file = self.file
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;
        file.sync_all()?;
        Ok(())
    }

    pub fn num_pages(&self) -> u32 {
        self.num_pages.load(Ordering::SeqCst)
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn validate_page_id(&self, page_id: PageId) -> Result<()> {
        if page_id == INVALID_PAGE_ID {
            return Err(DbError::InvalidPageId);
        }
        let current_pages = self.num_pages.load(Ordering::SeqCst);
        if page_id >= current_pages {
            return Err(DbError::PageOutOfBounds {
                page_id,
                num_pages: current_pages,
            });
        }
        Ok(())
    }

    fn check_not_free(&self, page_id: PageId) -> Result<()> {
        let free_list = self
            .free_list
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;
        if free_list.contains(&page_id) {
            return Err(DbError::PageFreed { page_id });
        }
        Ok(())
    }

    fn page_offset(page_id: PageId) -> u64 {
        page_id as u64 * PAGE_SIZE as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn create_test_dm() -> (DiskManager, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let dm = DiskManager::new(tmp.path()).unwrap();
        (dm, tmp)
    }

    #[test]
    fn test_new_empty_file() {
        let (dm, _tmp) = create_test_dm();
        assert_eq!(dm.num_pages(), 0);
    }

    #[test]
    fn test_allocate_increments_count() {
        let (dm, _tmp) = create_test_dm();

        let page0 = dm.allocate_page().unwrap();
        assert_eq!(page0, 0);
        assert_eq!(dm.num_pages(), 1);

        let page1 = dm.allocate_page().unwrap();
        assert_eq!(page1, 1);
        assert_eq!(dm.num_pages(), 2);
    }

    #[test]
    fn test_write_read_roundtrip() {
        let (dm, _tmp) = create_test_dm();
        dm.allocate_page().unwrap();

        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0xDE;
        data[1] = 0xAD;
        data[PAGE_SIZE - 1] = 0xFF;

        dm.write_page(0, &data).unwrap();
        let read_back = dm.read_page(0).unwrap();

        assert_eq!(read_back[0], 0xDE);
        assert_eq!(read_back[1], 0xAD);
        assert_eq!(read_back[PAGE_SIZE - 1], 0xFF);
        assert_eq!(read_back[2], 0x00);
    }

    #[test]
    fn test_invalid_page_id_error() {
        let (dm, _tmp) = create_test_dm();
        dm.allocate_page().unwrap();

        let result = dm.read_page(INVALID_PAGE_ID);
        assert!(matches!(result, Err(DbError::InvalidPageId)));
    }

    #[test]
    fn test_out_of_bounds_error() {
        let (dm, _tmp) = create_test_dm();
        dm.allocate_page().unwrap();

        let result = dm.read_page(5);
        assert!(matches!(result, Err(DbError::PageOutOfBounds { .. })));
    }

    #[test]
    fn test_multiple_pages_independent() {
        let (dm, _tmp) = create_test_dm();
        dm.allocate_page().unwrap();
        dm.allocate_page().unwrap();
        dm.allocate_page().unwrap();

        let mut data0 = [0u8; PAGE_SIZE];
        let mut data1 = [0u8; PAGE_SIZE];
        let mut data2 = [0u8; PAGE_SIZE];
        data0[0] = 0xAA;
        data1[0] = 0xBB;
        data2[0] = 0xCC;

        dm.write_page(0, &data0).unwrap();
        dm.write_page(1, &data1).unwrap();
        dm.write_page(2, &data2).unwrap();

        assert_eq!(dm.read_page(0).unwrap()[0], 0xAA);
        assert_eq!(dm.read_page(1).unwrap()[0], 0xBB);
        assert_eq!(dm.read_page(2).unwrap()[0], 0xCC);
    }

    #[test]
    fn test_free_page_and_reuse() {
        let (dm, _tmp) = create_test_dm();

        let p0 = dm.allocate_page().unwrap();
        let p1 = dm.allocate_page().unwrap();
        let p2 = dm.allocate_page().unwrap();
        assert_eq!((p0, p1, p2), (0, 1, 2));

        // Free page 1
        dm.free_page(1).unwrap();

        // Next allocation should reuse page 1
        let reused = dm.allocate_page().unwrap();
        assert_eq!(reused, 1);

        // Next allocation extends file
        let p3 = dm.allocate_page().unwrap();
        assert_eq!(p3, 3);
    }

    #[test]
    fn test_free_page_prevents_read_write() {
        let (dm, _tmp) = create_test_dm();
        dm.allocate_page().unwrap();

        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0x42;
        dm.write_page(0, &data).unwrap();

        dm.free_page(0).unwrap();

        assert!(matches!(
            dm.read_page(0),
            Err(DbError::PageFreed { page_id: 0 })
        ));
        assert!(matches!(
            dm.write_page(0, &data),
            Err(DbError::PageFreed { page_id: 0 })
        ));
    }

    #[test]
    fn test_double_free_error() {
        let (dm, _tmp) = create_test_dm();
        dm.allocate_page().unwrap();

        dm.free_page(0).unwrap();
        let result = dm.free_page(0);
        assert!(matches!(result, Err(DbError::DoubleFree { page_id: 0 })));
    }

    #[test]
    fn test_free_invalid_page() {
        let (dm, _tmp) = create_test_dm();
        dm.allocate_page().unwrap();

        assert!(matches!(
            dm.free_page(INVALID_PAGE_ID),
            Err(DbError::InvalidPageId)
        ));
        assert!(matches!(
            dm.free_page(99),
            Err(DbError::PageOutOfBounds { .. })
        ));
    }

    #[test]
    fn test_alloc_free_alloc_cycle() {
        let (dm, _tmp) = create_test_dm();

        // Allocate 5 pages
        for _ in 0..5 {
            dm.allocate_page().unwrap();
        }

        // Free pages 2, 4 (FIFO order)
        dm.free_page(2).unwrap();
        dm.free_page(4).unwrap();

        // Reallocate — should get 2, then 4
        assert_eq!(dm.allocate_page().unwrap(), 2);
        assert_eq!(dm.allocate_page().unwrap(), 4);

        // Then extends
        assert_eq!(dm.allocate_page().unwrap(), 5);
    }
}
