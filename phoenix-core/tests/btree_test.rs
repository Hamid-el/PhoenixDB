use phoenix_core::paging::{BufferPoolManager, ClockStrategy, DiskManager};
use phoenix_core::storage::BPlusTree;
use tempfile::NamedTempFile;

fn create_tree(pool_size: usize) -> (BPlusTree<'static, ClockStrategy, 64>, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    let bpm = Box::leak(Box::new(BufferPoolManager::new(
        pool_size,
        dm,
        ClockStrategy::new(pool_size),
    )));
    let tree = BPlusTree::<ClockStrategy, 64>::create(bpm).unwrap();
    (tree, tmp)
}

#[test]
fn test_insert_1000_sequential_keys() {
    let (tree, _tmp) = create_tree(100);

    for i in 0..1000u64 {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&i.to_le_bytes());
        tree.insert(i, &val).unwrap();
    }

    for i in 0..1000u64 {
        let result = tree.search(i).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, i, "Key {} has wrong value", i);
    }
}

#[test]
fn test_insert_1000_random_keys() {
    let (tree, _tmp) = create_tree(100);

    let mut keys: Vec<u64> = (0..1000).collect();
    let mut seed: u64 = 12345;
    for i in (1..keys.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let j = (seed >> 33) as usize % (i + 1);
        keys.swap(i, j);
    }

    for &k in &keys {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&k.to_le_bytes());
        tree.insert(k, &val).unwrap();
    }

    for &k in &keys {
        let result = tree.search(k).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, k, "Key {} has wrong value", k);
    }
}

#[test]
fn test_range_scan_full_tree() {
    let (tree, _tmp) = create_tree(100);

    for i in 0..500u64 {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&i.to_le_bytes());
        tree.insert(i, &val).unwrap();
    }

    let results = tree.range_scan(100, 399).unwrap();
    assert_eq!(results.len(), 300);

    for (idx, (k, v)) in results.iter().enumerate() {
        let expected_key = (idx + 100) as u64;
        assert_eq!(*k, expected_key);
        let stored = u64::from_le_bytes(v[..8].try_into().unwrap());
        assert_eq!(stored, expected_key);
    }
}

#[test]
fn test_tree_persistence() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    let root_id;
    {
        let dm = DiskManager::new(&path).unwrap();
        let bpm = Box::leak(Box::new(BufferPoolManager::new(
            50,
            dm,
            ClockStrategy::new(50),
        )));
        let tree = BPlusTree::<ClockStrategy, 64>::create(bpm).unwrap();

        for i in 0..200u64 {
            let mut val = [0u8; 64];
            val[..8].copy_from_slice(&i.to_le_bytes());
            tree.insert(i, &val).unwrap();
        }

        bpm.flush_all().unwrap();
        root_id = tree.root_page_id();
    }

    let dm2 = DiskManager::new(&path).unwrap();
    let bpm2 = Box::leak(Box::new(BufferPoolManager::new(
        50,
        dm2,
        ClockStrategy::new(50),
    )));
    let tree2 = BPlusTree::<ClockStrategy, 64>::open(bpm2, root_id);

    for i in 0..200u64 {
        let result = tree2.search(i).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, i, "Key {} wrong after reopen", i);
    }
}

#[test]
fn test_tree_with_small_pool() {
    let (tree, _tmp) = create_tree(10);

    for i in 0..200u64 {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&i.to_le_bytes());
        tree.insert(i, &val).unwrap();
    }

    for i in 0..200u64 {
        let result = tree.search(i).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, i, "Key {} wrong with small pool", i);
    }
}

#[test]
fn test_insert_reverse_order() {
    let (tree, _tmp) = create_tree(100);

    for i in (0..500u64).rev() {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&i.to_le_bytes());
        tree.insert(i, &val).unwrap();
    }

    for i in 0..500u64 {
        let result = tree.search(i).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, i, "Key {} wrong after reverse insert", i);
    }
}

#[test]
fn test_large_key_values() {
    let (tree, _tmp) = create_tree(100);

    let keys = [0u64, 1, u64::MAX - 1, u64::MAX, u64::MAX / 2];
    for &k in &keys {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&k.to_le_bytes());
        tree.insert(k, &val).unwrap();
    }

    for &k in &keys {
        let result = tree.search(k).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, k);
    }
}

#[test]
fn test_range_scan_empty_result() {
    let (tree, _tmp) = create_tree(100);

    for i in 0..100u64 {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&i.to_le_bytes());
        tree.insert(i * 10, &val).unwrap();
    }

    let results = tree.range_scan(5, 9).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_range_scan_single_key() {
    let (tree, _tmp) = create_tree(100);

    for i in 0..100u64 {
        let val = [0u8; 64];
        tree.insert(i * 10, &val).unwrap();
    }

    let results = tree.range_scan(50, 50).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, 50);
}

#[test]
fn test_range_scan_beyond_max() {
    let (tree, _tmp) = create_tree(100);

    for i in 0..50u64 {
        let val = [0u8; 64];
        tree.insert(i, &val).unwrap();
    }

    let results = tree.range_scan(0, 1000).unwrap();
    assert_eq!(results.len(), 50);
}

#[test]
fn test_interleaved_insert_pattern() {
    let (tree, _tmp) = create_tree(100);

    for i in 0..500u64 {
        let key = if i % 2 == 0 { i / 2 } else { 500 + i / 2 };
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&key.to_le_bytes());
        tree.insert(key, &val).unwrap();
    }

    for i in 0..500u64 {
        let key = if i % 2 == 0 { i / 2 } else { 500 + i / 2 };
        let result = tree.search(key).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, key);
    }
}

#[test]
fn test_many_splits_small_pool() {
    let (tree, _tmp) = create_tree(5);

    for i in 0..100u64 {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&i.to_le_bytes());
        tree.insert(i, &val).unwrap();
    }

    for i in 0..100u64 {
        let result = tree.search(i).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, i);
    }

    let results = tree.range_scan(10, 89).unwrap();
    assert_eq!(results.len(), 80);
}

#[test]
fn test_duplicate_key_does_not_corrupt() {
    let (tree, _tmp) = create_tree(100);

    let val1 = [0xAAu8; 64];
    let val2 = [0xBBu8; 64];

    tree.insert(42, &val1).unwrap();
    assert!(tree.insert(42, &val2).is_err());

    let result = tree.search(42).unwrap();
    assert_eq!(result[0], 0xAA);
}

#[test]
fn test_search_empty_tree() {
    let (tree, _tmp) = create_tree(100);
    assert!(tree.search(1).is_err());
}

#[test]
fn test_range_scan_empty_tree() {
    let (tree, _tmp) = create_tree(100);
    let results = tree.range_scan(0, 100).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_internal_node_split_reverse_insert() {
    let (tree, _tmp) = create_tree(200);
    let n = 12_000u64;

    for i in (0..n).rev() {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&i.to_le_bytes());
        tree.insert(i, &val).unwrap();
    }

    for i in 0..n {
        let result = tree.search(i).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, i, "Key {} wrong after internal split (reverse)", i);
    }

    let results = tree.range_scan(1000, 1999).unwrap();
    assert_eq!(results.len(), 1000);
}

#[test]
fn test_internal_node_split_sequential_insert() {
    let (tree, _tmp) = create_tree(200);
    let n = 12_000u64;

    for i in 0..n {
        let mut val = [0u8; 64];
        val[..8].copy_from_slice(&i.to_le_bytes());
        tree.insert(i, &val).unwrap();
    }

    for i in 0..n {
        let result = tree.search(i).unwrap();
        let stored = u64::from_le_bytes(result[..8].try_into().unwrap());
        assert_eq!(stored, i, "Key {} wrong after internal split (seq)", i);
    }
}
