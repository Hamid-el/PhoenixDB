use phoenix_core::paging::{BufferPoolManager, ClockStrategy, DiskManager};
use phoenix_core::storage::BitmapIndex;
use tempfile::NamedTempFile;

fn create_index(pool_size: usize) -> (BitmapIndex<'static, ClockStrategy>, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    let bpm = Box::leak(Box::new(BufferPoolManager::new(
        pool_size,
        dm,
        ClockStrategy::new(pool_size),
    )));
    let idx = BitmapIndex::create(bpm).unwrap();
    (idx, tmp)
}

#[test]
fn test_bitmap_index_insert_and_lookup() {
    let (idx, _tmp) = create_index(100);

    idx.insert(10, 0).unwrap();
    idx.insert(10, 5).unwrap();
    idx.insert(10, 99).unwrap();
    idx.insert(20, 1).unwrap();
    idx.insert(20, 99).unwrap();

    let bs10 = idx.lookup(10).unwrap();
    assert!(bs10.get(0));
    assert!(bs10.get(5));
    assert!(bs10.get(99));
    assert!(!bs10.get(1));
    assert!(!bs10.get(50));

    let bs20 = idx.lookup(20).unwrap();
    assert!(bs20.get(1));
    assert!(bs20.get(99));
    assert!(!bs20.get(0));
    assert!(!bs20.get(5));
}

#[test]
fn test_bitmap_index_and_operation() {
    let (idx, _tmp) = create_index(100);

    idx.insert(10, 1).unwrap();
    idx.insert(10, 3).unwrap();
    idx.insert(10, 5).unwrap();
    idx.insert(20, 3).unwrap();
    idx.insert(20, 5).unwrap();
    idx.insert(20, 7).unwrap();

    let bs10 = idx.lookup(10).unwrap();
    let bs20 = idx.lookup(20).unwrap();
    let result = bs10.and(&bs20);

    assert!(!result.get(1));
    assert!(result.get(3));
    assert!(result.get(5));
    assert!(!result.get(7));
}

#[test]
fn test_bitmap_index_or_operation() {
    let (idx, _tmp) = create_index(100);

    idx.insert(10, 1).unwrap();
    idx.insert(10, 3).unwrap();
    idx.insert(20, 3).unwrap();
    idx.insert(20, 7).unwrap();

    let bs10 = idx.lookup(10).unwrap();
    let bs20 = idx.lookup(20).unwrap();
    let result = bs10.or(&bs20);

    assert!(result.get(1));
    assert!(result.get(3));
    assert!(result.get(7));
    assert!(!result.get(0));
    assert!(!result.get(2));
}

#[test]
fn test_bitmap_index_nonexistent_key_returns_empty() {
    let (idx, _tmp) = create_index(100);

    idx.insert(10, 5).unwrap();

    let bs = idx.lookup(999).unwrap();
    assert_eq!(bs.count_ones(), 0);
}

#[test]
fn test_bitmap_index_many_values() {
    let (idx, _tmp) = create_index(100);

    for key in 0..50u64 {
        for row in (0..100u32).step_by(2) {
            idx.insert(key, row).unwrap();
        }
    }

    assert_eq!(idx.num_values().unwrap(), 50);

    let bs0 = idx.lookup(0).unwrap();
    assert_eq!(bs0.count_ones(), 50);
    assert!(bs0.get(0));
    assert!(!bs0.get(1));
    assert!(bs0.get(98));
}

#[test]
fn test_bitmap_index_large_row_ids() {
    let (idx, _tmp) = create_index(100);

    idx.insert(1, 0).unwrap();
    idx.insert(1, 10000).unwrap();
    idx.insert(1, 32767).unwrap();

    let bs = idx.lookup(1).unwrap();
    assert!(bs.get(0));
    assert!(bs.get(10000));
    assert!(bs.get(32767));
    assert!(!bs.get(5000));
    assert_eq!(bs.count_ones(), 3);
}
