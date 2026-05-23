use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use log::{debug, info};

use crate::paging::constants::{INVALID_PAGE_ID, PAGE_SIZE};
use crate::paging::disk_manager::DiskManager;
use crate::paging::error::{DbError, Result};
use crate::paging::types::{FrameId, PageId};

struct FrameMeta {
    page_id: PageId,
    pin_count: u32,
    is_dirty: bool,
    ref_bit: bool,
}

struct PoolState {
    page_table: HashMap<PageId, FrameId>,
    frames: Vec<FrameMeta>,
    clock_hand: usize,
    free_list: VecDeque<FrameId>,
}

pub struct BufferPoolManager {
    disk_manager: DiskManager,
    pool_size: usize,
    state: Mutex<PoolState>,
    page_data: Vec<RwLock<[u8; PAGE_SIZE]>>,
}

pub struct PageHandle<'a> {
    bpm: &'a BufferPoolManager,
    frame_id: FrameId,
    page_id: PageId,
}

impl<'a> PageHandle<'a> {
    pub fn read(&self) -> RwLockReadGuard<'_, [u8; PAGE_SIZE]> {
        self.bpm.page_data[self.frame_id as usize]
            .read()
            .expect("RwLock poisoned")
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, [u8; PAGE_SIZE]> {
        self.bpm.page_data[self.frame_id as usize]
            .write()
            .expect("RwLock poisoned")
    }

    pub fn page_id(&self) -> PageId {
        self.page_id
    }
}

impl BufferPoolManager {
    pub fn new(pool_size: usize, disk_manager: DiskManager) -> Self {
        let frames: Vec<FrameMeta> = (0..pool_size)
            .map(|_| FrameMeta {
                page_id: INVALID_PAGE_ID,
                pin_count: 0,
                is_dirty: false,
                ref_bit: false,
            })
            .collect();

        let free_list: VecDeque<FrameId> = (0..pool_size as FrameId).collect();

        let page_data: Vec<RwLock<[u8; PAGE_SIZE]>> = (0..pool_size)
            .map(|_| RwLock::new([0u8; PAGE_SIZE]))
            .collect();

        info!("BufferPoolManager created with {} frames", pool_size);

        Self {
            disk_manager,
            pool_size,
            state: Mutex::new(PoolState {
                page_table: HashMap::new(),
                frames,
                clock_hand: 0,
                free_list,
            }),
            page_data,
        }
    }

    pub fn fetch_page(&self, page_id: PageId) -> Result<PageHandle<'_>> {
        if page_id == INVALID_PAGE_ID {
            return Err(DbError::InvalidPageId);
        }

        let mut state = self.state.lock().map_err(|e| DbError::Internal(e.to_string()))?;

        // Cache hit
        if let Some(&frame_id) = state.page_table.get(&page_id) {
            state.frames[frame_id as usize].pin_count += 1;
            state.frames[frame_id as usize].ref_bit = true;
            debug!("fetch_page({}) -> cache hit, frame {}", page_id, frame_id);
            return Ok(PageHandle { bpm: self, frame_id, page_id });
        }

        // Cache miss — need a frame
        let frame_id = self.find_victim(&mut state)?;

        // Evict old page if frame was occupied
        let old_page_id = state.frames[frame_id as usize].page_id;
        if old_page_id != INVALID_PAGE_ID {
            if state.frames[frame_id as usize].is_dirty {
                let data = *self.page_data[frame_id as usize]
                    .read()
                    .expect("RwLock poisoned");
                self.disk_manager.write_page(old_page_id, &data)?;
            }
            state.page_table.remove(&old_page_id);
        }

        // Read new page from disk
        let data = self.disk_manager.read_page(page_id)?;
        {
            let mut frame_data = self.page_data[frame_id as usize]
                .write()
                .expect("RwLock poisoned");
            *frame_data = data;
        }

        // Update metadata
        state.frames[frame_id as usize] = FrameMeta {
            page_id,
            pin_count: 1,
            is_dirty: false,
            ref_bit: true,
        };
        state.page_table.insert(page_id, frame_id);

        debug!("fetch_page({}) -> cache miss, loaded into frame {}", page_id, frame_id);
        Ok(PageHandle { bpm: self, frame_id, page_id })
    }

    pub fn unpin_page(&self, page_id: PageId, is_dirty: bool) -> Result<()> {
        let mut state = self.state.lock().map_err(|e| DbError::Internal(e.to_string()))?;

        let &frame_id = state.page_table.get(&page_id)
            .ok_or(DbError::PageNotInPool { page_id })?;

        let meta = &mut state.frames[frame_id as usize];
        debug_assert!(meta.pin_count > 0, "unpin_page called with pin_count == 0");

        if meta.pin_count > 0 {
            meta.pin_count -= 1;
        }
        if is_dirty {
            meta.is_dirty = true;
        }

        debug!("unpin_page({}) -> pin_count={}, dirty={}", page_id, meta.pin_count, meta.is_dirty);
        Ok(())
    }

    pub fn new_page(&self) -> Result<PageHandle<'_>> {
        let mut state = self.state.lock().map_err(|e| DbError::Internal(e.to_string()))?;

        let frame_id = self.find_victim(&mut state)?;

        // Evict old page if frame was occupied
        let old_page_id = state.frames[frame_id as usize].page_id;
        if old_page_id != INVALID_PAGE_ID {
            if state.frames[frame_id as usize].is_dirty {
                let data = *self.page_data[frame_id as usize]
                    .read()
                    .expect("RwLock poisoned");
                self.disk_manager.write_page(old_page_id, &data)?;
            }
            state.page_table.remove(&old_page_id);
        }

        // Allocate new page on disk
        let new_page_id = self.disk_manager.allocate_page()?;

        // Zero out frame data
        {
            let mut frame_data = self.page_data[frame_id as usize]
                .write()
                .expect("RwLock poisoned");
            *frame_data = [0u8; PAGE_SIZE];
        }

        // Update metadata
        state.frames[frame_id as usize] = FrameMeta {
            page_id: new_page_id,
            pin_count: 1,
            is_dirty: false,
            ref_bit: true,
        };
        state.page_table.insert(new_page_id, frame_id);

        info!("new_page() -> page_id={}, frame_id={}", new_page_id, frame_id);
        Ok(PageHandle { bpm: self, frame_id, page_id: new_page_id })
    }

    pub fn flush_page(&self, page_id: PageId) -> Result<()> {
        let mut state = self.state.lock().map_err(|e| DbError::Internal(e.to_string()))?;

        let &frame_id = state.page_table.get(&page_id)
            .ok_or(DbError::PageNotInPool { page_id })?;

        if !state.frames[frame_id as usize].is_dirty {
            return Ok(());
        }

        let data = *self.page_data[frame_id as usize]
            .read()
            .expect("RwLock poisoned");
        self.disk_manager.write_page(page_id, &data)?;
        state.frames[frame_id as usize].is_dirty = false;

        debug!("flush_page({}) -> written to disk", page_id);
        Ok(())
    }

    pub fn flush_all(&self) -> Result<()> {
        let mut state = self.state.lock().map_err(|e| DbError::Internal(e.to_string()))?;

        for frame_id in 0..self.pool_size {
            let meta = &state.frames[frame_id];
            if meta.page_id == INVALID_PAGE_ID || !meta.is_dirty {
                continue;
            }

            let page_id = meta.page_id;
            let data = *self.page_data[frame_id]
                .read()
                .expect("RwLock poisoned");
            self.disk_manager.write_page(page_id, &data)?;
            state.frames[frame_id].is_dirty = false;
        }

        info!("flush_all() -> all dirty pages written to disk");
        Ok(())
    }

    fn find_victim(&self, state: &mut PoolState) -> Result<FrameId> {
        // Prefer free frames
        if let Some(frame_id) = state.free_list.pop_front() {
            return Ok(frame_id);
        }

        // Clock sweep — at most 2 full rotations
        let max_iterations = 2 * self.pool_size;

        for _ in 0..max_iterations {
            let frame_id = state.clock_hand;
            state.clock_hand = (state.clock_hand + 1) % self.pool_size;

            let meta = &mut state.frames[frame_id];

            if meta.pin_count > 0 {
                continue;
            }

            if meta.page_id == INVALID_PAGE_ID {
                continue;
            }

            if meta.ref_bit {
                meta.ref_bit = false;
                continue;
            }

            return Ok(frame_id as FrameId);
        }

        Err(DbError::BufferPoolFull { pool_size: self.pool_size })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn create_test_bpm(pool_size: usize) -> (BufferPoolManager, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let dm = DiskManager::new(tmp.path()).unwrap();
        let bpm = BufferPoolManager::new(pool_size, dm);
        (bpm, tmp)
    }

    #[test]
    fn test_new_page_allocates_and_pins() {
        let (bpm, _tmp) = create_test_bpm(4);
        let handle = bpm.new_page().unwrap();
        assert_eq!(handle.page_id(), 0);

        let data = handle.read();
        assert_eq!(data[0], 0);
        drop(data);

        bpm.unpin_page(0, false).unwrap();
    }

    #[test]
    fn test_fetch_cache_hit() {
        let (bpm, _tmp) = create_test_bpm(4);

        let handle = bpm.new_page().unwrap();
        let page_id = handle.page_id();
        {
            let mut data = handle.write();
            data[0] = 0xAB;
        }
        bpm.unpin_page(page_id, true).unwrap();

        // Fetch same page — should be a cache hit
        let handle2 = bpm.fetch_page(page_id).unwrap();
        let data = handle2.read();
        assert_eq!(data[0], 0xAB);
        drop(data);
        bpm.unpin_page(page_id, false).unwrap();
    }

    #[test]
    fn test_unpin_decrements_pin_count() {
        let (bpm, _tmp) = create_test_bpm(4);
        let handle = bpm.new_page().unwrap();
        let page_id = handle.page_id();

        // Fetch again — pin_count = 2
        let _handle2 = bpm.fetch_page(page_id).unwrap();

        bpm.unpin_page(page_id, false).unwrap();
        bpm.unpin_page(page_id, false).unwrap();
    }

    #[test]
    fn test_flush_page_writes_to_disk() {
        let (bpm, tmp) = create_test_bpm(4);

        let handle = bpm.new_page().unwrap();
        let page_id = handle.page_id();
        {
            let mut data = handle.write();
            data[0] = 0xDE;
            data[1] = 0xAD;
        }
        bpm.unpin_page(page_id, true).unwrap();
        bpm.flush_page(page_id).unwrap();

        // Verify via a fresh DiskManager
        let dm2 = DiskManager::new(tmp.path()).unwrap();
        let page = dm2.read_page(page_id).unwrap();
        assert_eq!(page[0], 0xDE);
        assert_eq!(page[1], 0xAD);
    }

    #[test]
    fn test_clock_evicts_unreferenced() {
        let (bpm, _tmp) = create_test_bpm(3);

        // Fill pool with 3 pages
        for _ in 0..3 {
            let handle = bpm.new_page().unwrap();
            bpm.unpin_page(handle.page_id(), false).unwrap();
        }

        // Allocate a 4th page — must evict one
        let handle = bpm.new_page().unwrap();
        assert_eq!(handle.page_id(), 3);
        bpm.unpin_page(handle.page_id(), false).unwrap();
    }

    #[test]
    fn test_clock_second_chance() {
        let (bpm, _tmp) = create_test_bpm(3);

        // Fill pool: pages 0, 1, 2
        for _ in 0..3 {
            let handle = bpm.new_page().unwrap();
            bpm.unpin_page(handle.page_id(), false).unwrap();
        }

        // Fetch page 0 again — sets ref_bit on its frame
        let handle = bpm.fetch_page(0).unwrap();
        bpm.unpin_page(handle.page_id(), false).unwrap();

        // Allocate page 3 — clock should skip page 0 (ref_bit=1), evict page 1
        let handle = bpm.new_page().unwrap();
        assert_eq!(handle.page_id(), 3);
        bpm.unpin_page(handle.page_id(), false).unwrap();

        // Page 0 should still be in pool (was given second chance)
        let handle = bpm.fetch_page(0).unwrap();
        let data = handle.read();
        assert_eq!(data[0], 0); // still accessible
        drop(data);
        bpm.unpin_page(0, false).unwrap();
    }

    #[test]
    fn test_clock_skips_pinned() {
        let (bpm, _tmp) = create_test_bpm(3);

        // Fill pool
        let handle0 = bpm.new_page().unwrap(); // page 0 — stays pinned
        let handle1 = bpm.new_page().unwrap();
        bpm.unpin_page(handle1.page_id(), false).unwrap();
        let handle2 = bpm.new_page().unwrap();
        bpm.unpin_page(handle2.page_id(), false).unwrap();

        // Allocate new page — should NOT evict page 0 (still pinned)
        let handle3 = bpm.new_page().unwrap();
        assert_eq!(handle3.page_id(), 3);
        bpm.unpin_page(handle3.page_id(), false).unwrap();

        // Page 0 still in pool
        assert_eq!(handle0.page_id(), 0);
        bpm.unpin_page(0, false).unwrap();
    }

    #[test]
    fn test_all_pinned_returns_error() {
        let (bpm, _tmp) = create_test_bpm(2);

        let _h0 = bpm.new_page().unwrap(); // pinned
        let _h1 = bpm.new_page().unwrap(); // pinned

        // Pool full, all pinned
        let result = bpm.new_page();
        assert!(matches!(result, Err(DbError::BufferPoolFull { pool_size: 2 })));
    }

    #[test]
    fn test_dirty_eviction_flushes() {
        let (bpm, tmp) = create_test_bpm(2);

        // Page 0: write dirty data
        let handle = bpm.new_page().unwrap();
        {
            let mut data = handle.write();
            data[0] = 0xFF;
        }
        bpm.unpin_page(0, true).unwrap();

        // Page 1
        let handle = bpm.new_page().unwrap();
        bpm.unpin_page(handle.page_id(), false).unwrap();

        // Page 2 — evicts page 0 (dirty) which should be flushed to disk
        let handle = bpm.new_page().unwrap();
        bpm.unpin_page(handle.page_id(), false).unwrap();

        // Verify page 0 was written to disk
        let dm2 = DiskManager::new(tmp.path()).unwrap();
        let page = dm2.read_page(0).unwrap();
        assert_eq!(page[0], 0xFF);
    }

    #[test]
    fn test_flush_all() {
        let (bpm, tmp) = create_test_bpm(4);

        for i in 0u8..3 {
            let handle = bpm.new_page().unwrap();
            {
                let mut data = handle.write();
                data[0] = i + 1;
            }
            bpm.unpin_page(handle.page_id(), true).unwrap();
        }

        bpm.flush_all().unwrap();

        let dm2 = DiskManager::new(tmp.path()).unwrap();
        for i in 0u32..3 {
            let page = dm2.read_page(i).unwrap();
            assert_eq!(page[0], (i + 1) as u8);
        }
    }

    #[test]
    fn test_invalid_page_id_error() {
        let (bpm, _tmp) = create_test_bpm(4);
        let result = bpm.fetch_page(INVALID_PAGE_ID);
        assert!(matches!(result, Err(DbError::InvalidPageId)));
    }

    #[test]
    fn test_page_not_in_pool_error() {
        let (bpm, _tmp) = create_test_bpm(4);
        let result = bpm.unpin_page(42, false);
        assert!(matches!(result, Err(DbError::PageNotInPool { page_id: 42 })));
    }
}
