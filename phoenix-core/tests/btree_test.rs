use phoenix_core::paging::{BufferPoolManager, ClockStrategy, DiskManager};
use phoenix_core::storage::BPlusTree;
use tempfile::NamedTempFile;

fn create_tree(pool_size: usize) -> (BPlusTree<'static, ClockStrategy, 64>, NamedTempFile) {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    let bpm = Box::leak(Box::new(BufferPoolManager::new(pool_size, dm, ClockStrategy::new(pool_size))));
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
        let bpm = Box::leak(Box::new(BufferPoolManager::new(50, dm, ClockStrategy::new(50))));
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
    let bpm2 = Box::leak(Box::new(BufferPoolManager::new(50, dm2, ClockStrategy::new(50))));
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
