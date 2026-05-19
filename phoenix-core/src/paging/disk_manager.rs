use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use log::{debug, info};

use crate::paging::error::{DbError, Result};
use crate::paging::constants::{INVALID_PAGE_ID, PAGE_SIZE};
use crate::paging::types::PageId;

pub struct DiskManager {
    db_path: PathBuf,
    file: Mutex<File>,
    num_pages: AtomicU32,
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

        info!("DiskManager opened '{}' with {} pages", db_path.display(), num_pages);

        Ok(Self {
            db_path,
            file: Mutex::new(file),
            num_pages: AtomicU32::new(num_pages),
        })
    }

    pub fn read_page(&self, page_id: PageId) -> Result<[u8; PAGE_SIZE]> {
        self.validate_page_id(page_id)?;

        let offset = Self::page_offset(page_id);
        let mut buf = [0u8; PAGE_SIZE];

        let mut file = self.file.lock().map_err(|e| DbError::Internal(e.to_string()))?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut buf)?;

        debug!("Read page {}", page_id);
        Ok(buf)
    }

    pub fn write_page(&self, page_id: PageId, data: &[u8; PAGE_SIZE]) -> Result<()> {
        self.validate_page_id(page_id)?;

        let offset = Self::page_offset(page_id);

        let mut file = self.file.lock().map_err(|e| DbError::Internal(e.to_string()))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)?;
        file.flush()?;

        debug!("Wrote page {}", page_id);
        Ok(())
    }

    pub fn allocate_page(&self) -> Result<PageId> {
        let new_page_id = self.num_pages.fetch_add(1, Ordering::SeqCst);
        let offset = Self::page_offset(new_page_id);
        let zeroed = [0u8; PAGE_SIZE];

        let mut file = self.file.lock().map_err(|e| DbError::Internal(e.to_string()))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&zeroed)?;
        file.flush()?;

        info!("Allocated page {}", new_page_id);
        Ok(new_page_id)
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
}
