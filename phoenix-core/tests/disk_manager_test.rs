extern crate alloc;

use std::thread;
use alloc::vec::Vec;
use core::{assert, assert_eq, matches};
use core::iter::{IntoIterator, Iterator};
use core::prelude::rust_2024::Err;


use phoenix_core::paging::{DbError, DiskManager, INVALID_PAGE_ID, PAGE_SIZE};
use tempfile::NamedTempFile;

#[test]
fn test_persistence_across_instances() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    {
        let dm = DiskManager::new(&path).unwrap();
        dm.allocate_page().unwrap();

        let mut data = [0u8; PAGE_SIZE];
        data[0] = 0x42;
        data[100] = 0x99;
        dm.write_page(0, &data).unwrap();
    }

    let dm2 = DiskManager::new(&path).unwrap();
    assert_eq!(dm2.num_pages(), 1);

    let read_back = dm2.read_page(0).unwrap();
    assert_eq!(read_back[0], 0x42);
    assert_eq!(read_back[100], 0x99);
}

#[test]
fn test_allocate_many_pages() {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();

    for i in 0u32..100 {
        let page_id = dm.allocate_page().unwrap();
        assert_eq!(page_id, i);

        let mut data = [0u8; PAGE_SIZE];
        let bytes = i.to_le_bytes();
        data[..4].copy_from_slice(&bytes);
        dm.write_page(page_id, &data).unwrap();
    }

    assert_eq!(dm.num_pages(), 100);

    for i in 0u32..100 {
        let page = dm.read_page(i).unwrap();
        let stored = u32::from_le_bytes([page[0], page[1], page[2], page[3]]);
        assert_eq!(stored, i);
    }
}

#[test]
fn test_error_on_invalid_page_id() {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    dm.allocate_page().unwrap();

    let result = dm.read_page(INVALID_PAGE_ID);
    assert!(matches!(result, Err(DbError::InvalidPageId)));

    let data = [0u8; PAGE_SIZE];
    let result = dm.write_page(INVALID_PAGE_ID, &data);
    assert!(matches!(result, Err(DbError::InvalidPageId)));
}

#[test]
fn test_error_on_out_of_bounds() {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    dm.allocate_page().unwrap();
    dm.allocate_page().unwrap();
    dm.allocate_page().unwrap();

    let result = dm.read_page(5);
    assert!(matches!(
        result,
        Err(DbError::PageOutOfBounds { page_id: 5, num_pages: 3 })
    ));
}

#[test]
fn test_concurrent_access() {
    let tmp = NamedTempFile::new().unwrap();
    let dm = std::sync::Arc::new(DiskManager::new(tmp.path()).unwrap());

    let handles: Vec<_> = (0u8..10)
        .map(|i| {
            let dm = dm.clone();
            thread::spawn(move || {
                let page_id = dm.allocate_page().unwrap();
                let mut data = [0u8; PAGE_SIZE];
                data[0] = i;
                dm.write_page(page_id, &data).unwrap();
                (page_id, i)
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    assert_eq!(dm.num_pages(), 10);

    for (page_id, expected_byte) in results {
        let page = dm.read_page(page_id).unwrap();
        assert_eq!(page[0], expected_byte);
    }
}
