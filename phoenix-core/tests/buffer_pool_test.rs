use std::sync::Arc;
use std::thread;

use phoenix_core::paging::{BufferPoolManager, ClockStrategy, DiskManager, PAGE_SIZE};
use tempfile::NamedTempFile;

#[test]
fn test_pool_smaller_than_pages() {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();

    for _ in 0..10 {
        dm.allocate_page().unwrap();
    }

    for i in 0u32..10 {
        let mut data = [0u8; PAGE_SIZE];
        let bytes = i.to_le_bytes();
        data[..4].copy_from_slice(&bytes);
        dm.write_page(i, &data).unwrap();
    }

    let dm2 = DiskManager::new(tmp.path()).unwrap();
    let bpm = BufferPoolManager::new(3, dm2, ClockStrategy::new(3));

    for i in 0u32..10 {
        let handle = bpm.fetch_page(i).unwrap();
        let data = handle.read();
        let stored = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        assert_eq!(stored, i, "Page {} has wrong content", i);
        drop(data);
        bpm.unpin_page(i, false).unwrap();
    }
}

#[test]
fn test_working_set_stays_cached() {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    let bpm = BufferPoolManager::new(4, dm, ClockStrategy::new(4));

    for _ in 0..4 {
        let handle = bpm.new_page().unwrap();
        bpm.unpin_page(handle.page_id(), false).unwrap();
    }

    for _ in 0..100 {
        for i in 0u32..4 {
            let handle = bpm.fetch_page(i).unwrap();
            drop(handle.read());
            bpm.unpin_page(i, false).unwrap();
        }
    }
}

#[test]
fn test_concurrent_buffer_pool_access() {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    let bpm = Arc::new(BufferPoolManager::new(8, dm, ClockStrategy::new(8)));

    for _ in 0..8 {
        let handle = bpm.new_page().unwrap();
        bpm.unpin_page(handle.page_id(), false).unwrap();
    }

    let handles: Vec<_> = (0u32..4)
        .map(|i| {
            let bpm = bpm.clone();
            thread::spawn(move || {
                for _ in 0..50 {
                    let page_id = i * 2;
                    let handle = bpm.fetch_page(page_id).unwrap();
                    {
                        let mut data = handle.write();
                        data[0] = data[0].wrapping_add(1);
                    }
                    bpm.unpin_page(page_id, true).unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    bpm.flush_all().unwrap();

    for i in 0u32..4 {
        let page_id = i * 2;
        let handle = bpm.fetch_page(page_id).unwrap();
        let data = handle.read();
        assert_eq!(data[0], 50);
        drop(data);
        bpm.unpin_page(page_id, false).unwrap();
    }
}
