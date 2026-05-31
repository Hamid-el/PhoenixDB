use phoenix_core::paging::{BufferPoolManager, ClockStrategy, DiskManager};
use phoenix_core::query::join::HashJoin;
use phoenix_core::query::scan::TableScan;
use phoenix_core::query::sort::{Sort, SortConfig};
use phoenix_core::query::Operator;
use phoenix_core::storage::BPlusTree;
use tempfile::NamedTempFile;

fn setup_tree(n: u64) -> (&'static BufferPoolManager<ClockStrategy>, u32) {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    let bpm = Box::leak(Box::new(BufferPoolManager::new(
        200,
        dm,
        ClockStrategy::new(200),
    )));
    let tree = BPlusTree::<ClockStrategy, 64>::create(bpm).unwrap();

    for i in 0..n {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&(i * 10).to_le_bytes());
        tree.insert(i, &value).unwrap();
    }

    let root = tree.root_page_id();
    (bpm, root)
}

fn key_from_record(record: &Vec<u8>) -> u64 {
    u64::from_le_bytes(record[0..8].try_into().unwrap())
}

#[test]
fn test_cursor_full_scan() {
    let (bpm, root) = setup_tree(100);

    let mut scan = TableScan::<ClockStrategy, 64>::new(bpm, root);
    scan.open().unwrap();

    let mut count = 0u64;
    let mut prev_key = 0u64;
    while let Some(record) = scan.next().unwrap() {
        let key = key_from_record(&record);
        if count > 0 {
            assert!(key > prev_key, "keys must be in ascending order");
        }
        assert_eq!(record.len(), 72);
        prev_key = key;
        count += 1;
    }
    assert_eq!(count, 100);
    scan.close().unwrap();
}

#[test]
fn test_cursor_range_scan() {
    let (bpm, root) = setup_tree(1000);

    let mut scan = TableScan::<ClockStrategy, 64>::range(bpm, root, 100, 200);
    scan.open().unwrap();

    let mut count = 0u64;
    while let Some(record) = scan.next().unwrap() {
        let key = key_from_record(&record);
        assert!(key >= 100 && key <= 200);
        count += 1;
    }
    assert_eq!(count, 101);
    scan.close().unwrap();
}

#[test]
fn test_cursor_empty_tree() {
    let (bpm, root) = setup_tree(0);

    let mut scan = TableScan::<ClockStrategy, 64>::new(bpm, root);
    scan.open().unwrap();

    assert!(scan.next().unwrap().is_none());
    scan.close().unwrap();
}

#[test]
fn test_table_scan_basic() {
    let (bpm, root) = setup_tree(1000);

    let mut scan = TableScan::<ClockStrategy, 64>::new(bpm, root);
    scan.open().unwrap();

    let mut results = Vec::new();
    while let Some(record) = scan.next().unwrap() {
        results.push(record);
    }
    scan.close().unwrap();

    assert_eq!(results.len(), 1000);
    for (i, record) in results.iter().enumerate() {
        let key = key_from_record(record);
        assert_eq!(key, i as u64);
        let payload = u64::from_le_bytes(record[8..16].try_into().unwrap());
        assert_eq!(payload, i as u64 * 10);
    }
}

#[test]
fn test_sort_operator() {
    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    let bpm = Box::leak(Box::new(BufferPoolManager::new(
        200,
        dm,
        ClockStrategy::new(200),
    )));
    let tree = BPlusTree::<ClockStrategy, 64>::create(bpm).unwrap();
    let root = tree.root_page_id();

    let mut seed: u64 = 12345;
    let n = 500u64;
    let mut keys: Vec<u64> = Vec::new();
    for _ in 0..n {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let key = seed % 100_000;
        keys.push(key);
    }
    keys.sort();
    keys.dedup();

    for &key in &keys {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&key.to_le_bytes());
        tree.insert(key, &value).unwrap();
    }

    let scan = TableScan::<ClockStrategy, 64>::new(bpm, root);
    let sort_key_fn: fn(&Vec<u8>) -> u64 = |record| {
        u64::from_le_bytes(record[8..16].try_into().unwrap())
    };

    let mut sort_op = Sort::new(
        Box::new(scan),
        sort_key_fn,
        SortConfig::default(),
    );
    sort_op.open().unwrap();

    let mut prev_sort_key: Option<u64> = None;
    let mut count = 0;
    while let Some(record) = sort_op.next().unwrap() {
        let sk = sort_key_fn(&record);
        if let Some(prev) = prev_sort_key {
            assert!(sk >= prev, "sort output not in order");
        }
        prev_sort_key = Some(sk);
        count += 1;
    }
    sort_op.close().unwrap();
    assert_eq!(count, keys.len());
}

#[test]
fn test_hash_join_basic() {
    let tmp_left = NamedTempFile::new().unwrap();
    let dm_left = DiskManager::new(tmp_left.path()).unwrap();
    let bpm_left = Box::leak(Box::new(BufferPoolManager::new(
        100,
        dm_left,
        ClockStrategy::new(100),
    )));
    let tree_left = BPlusTree::<ClockStrategy, 64>::create(bpm_left).unwrap();
    let root_left = tree_left.root_page_id();

    for i in 0..50u64 {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&(i * 2).to_le_bytes());
        tree_left.insert(i, &value).unwrap();
    }

    let tmp_right = NamedTempFile::new().unwrap();
    let dm_right = DiskManager::new(tmp_right.path()).unwrap();
    let bpm_right = Box::leak(Box::new(BufferPoolManager::new(
        100,
        dm_right,
        ClockStrategy::new(100),
    )));
    let tree_right = BPlusTree::<ClockStrategy, 64>::create(bpm_right).unwrap();
    let root_right = tree_right.root_page_id();

    for i in 0..50u64 {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&(i * 3).to_le_bytes());
        tree_right.insert(i, &value).unwrap();
    }

    let left_scan = TableScan::<ClockStrategy, 64>::new(bpm_left, root_left);
    let right_scan = TableScan::<ClockStrategy, 64>::new(bpm_right, root_right);

    let left_key_fn: fn(&Vec<u8>) -> u64 = |r| u64::from_le_bytes(r[0..8].try_into().unwrap());
    let right_key_fn: fn(&Vec<u8>) -> u64 = |r| u64::from_le_bytes(r[0..8].try_into().unwrap());

    let mut join = HashJoin::new(
        Box::new(left_scan),
        Box::new(right_scan),
        left_key_fn,
        right_key_fn,
    );
    join.open().unwrap();

    let mut results = Vec::new();
    while let Some(record) = join.next().unwrap() {
        assert_eq!(record.len(), 144);
        results.push(record);
    }
    join.close().unwrap();

    assert_eq!(results.len(), 50);
    for record in &results {
        let left_key = u64::from_le_bytes(record[0..8].try_into().unwrap());
        let right_key = u64::from_le_bytes(record[72..80].try_into().unwrap());
        assert_eq!(left_key, right_key);
    }
}

#[test]
fn test_hash_join_no_matches() {
    let tmp_left = NamedTempFile::new().unwrap();
    let dm_left = DiskManager::new(tmp_left.path()).unwrap();
    let bpm_left = Box::leak(Box::new(BufferPoolManager::new(
        100,
        dm_left,
        ClockStrategy::new(100),
    )));
    let tree_left = BPlusTree::<ClockStrategy, 64>::create(bpm_left).unwrap();
    let root_left = tree_left.root_page_id();

    for i in 0..10u64 {
        let value = [0u8; 64];
        tree_left.insert(i, &value).unwrap();
    }

    let tmp_right = NamedTempFile::new().unwrap();
    let dm_right = DiskManager::new(tmp_right.path()).unwrap();
    let bpm_right = Box::leak(Box::new(BufferPoolManager::new(
        100,
        dm_right,
        ClockStrategy::new(100),
    )));
    let tree_right = BPlusTree::<ClockStrategy, 64>::create(bpm_right).unwrap();
    let root_right = tree_right.root_page_id();

    for i in 100..110u64 {
        let value = [0u8; 64];
        tree_right.insert(i, &value).unwrap();
    }

    let left_scan = TableScan::<ClockStrategy, 64>::new(bpm_left, root_left);
    let right_scan = TableScan::<ClockStrategy, 64>::new(bpm_right, root_right);

    let key_fn: fn(&Vec<u8>) -> u64 = |r| u64::from_le_bytes(r[0..8].try_into().unwrap());

    let mut join = HashJoin::new(
        Box::new(left_scan),
        Box::new(right_scan),
        key_fn,
        key_fn,
    );
    join.open().unwrap();

    assert!(join.next().unwrap().is_none());
    join.close().unwrap();
}

#[test]
fn test_hash_join_many_matches() {
    let tmp_left = NamedTempFile::new().unwrap();
    let dm_left = DiskManager::new(tmp_left.path()).unwrap();
    let bpm_left = Box::leak(Box::new(BufferPoolManager::new(
        100,
        dm_left,
        ClockStrategy::new(100),
    )));
    let tree_left = BPlusTree::<ClockStrategy, 64>::create(bpm_left).unwrap();
    let root_left = tree_left.root_page_id();

    for i in 0..20u64 {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&(i % 5).to_le_bytes());
        tree_left.insert(i, &value).unwrap();
    }

    let tmp_right = NamedTempFile::new().unwrap();
    let dm_right = DiskManager::new(tmp_right.path()).unwrap();
    let bpm_right = Box::leak(Box::new(BufferPoolManager::new(
        100,
        dm_right,
        ClockStrategy::new(100),
    )));
    let tree_right = BPlusTree::<ClockStrategy, 64>::create(bpm_right).unwrap();
    let root_right = tree_right.root_page_id();

    for i in 0..10u64 {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&(i % 5).to_le_bytes());
        tree_right.insert(i, &value).unwrap();
    }

    let left_key_fn: fn(&Vec<u8>) -> u64 = |r| {
        u64::from_le_bytes(r[8..16].try_into().unwrap())
    };
    let right_key_fn: fn(&Vec<u8>) -> u64 = |r| {
        u64::from_le_bytes(r[8..16].try_into().unwrap())
    };

    let left_scan = TableScan::<ClockStrategy, 64>::new(bpm_left, root_left);
    let right_scan = TableScan::<ClockStrategy, 64>::new(bpm_right, root_right);

    let mut join = HashJoin::new(
        Box::new(left_scan),
        Box::new(right_scan),
        left_key_fn,
        right_key_fn,
    );
    join.open().unwrap();

    let mut count = 0;
    while join.next().unwrap().is_some() {
        count += 1;
    }
    join.close().unwrap();

    assert_eq!(count, 40);
}

#[test]
fn test_pipeline_scan_sort_join() {
    let tmp_left = NamedTempFile::new().unwrap();
    let dm_left = DiskManager::new(tmp_left.path()).unwrap();
    let bpm_left = Box::leak(Box::new(BufferPoolManager::new(
        200,
        dm_left,
        ClockStrategy::new(200),
    )));
    let tree_left = BPlusTree::<ClockStrategy, 64>::create(bpm_left).unwrap();
    let root_left = tree_left.root_page_id();

    for i in 0..100u64 {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&(100 - i).to_le_bytes());
        tree_left.insert(i, &value).unwrap();
    }

    let tmp_right = NamedTempFile::new().unwrap();
    let dm_right = DiskManager::new(tmp_right.path()).unwrap();
    let bpm_right = Box::leak(Box::new(BufferPoolManager::new(
        200,
        dm_right,
        ClockStrategy::new(200),
    )));
    let tree_right = BPlusTree::<ClockStrategy, 64>::create(bpm_right).unwrap();
    let root_right = tree_right.root_page_id();

    for i in 1..51u64 {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&i.to_le_bytes());
        tree_right.insert(i, &value).unwrap();
    }

    let left_scan = TableScan::<ClockStrategy, 64>::new(bpm_left, root_left);
    let sort_key_fn: fn(&Vec<u8>) -> u64 = |r| {
        u64::from_le_bytes(r[8..16].try_into().unwrap())
    };
    let sorted_left = Sort::new(Box::new(left_scan), sort_key_fn, SortConfig::default());

    let right_scan = TableScan::<ClockStrategy, 64>::new(bpm_right, root_right);

    let left_join_key: fn(&Vec<u8>) -> u64 = |r| {
        u64::from_le_bytes(r[8..16].try_into().unwrap())
    };
    let right_join_key: fn(&Vec<u8>) -> u64 = |r| {
        u64::from_le_bytes(r[8..16].try_into().unwrap())
    };

    let mut join = HashJoin::new(
        Box::new(sorted_left),
        Box::new(right_scan),
        left_join_key,
        right_join_key,
    );
    join.open().unwrap();

    let mut results = Vec::new();
    while let Some(record) = join.next().unwrap() {
        results.push(record);
    }
    join.close().unwrap();

    assert_eq!(results.len(), 50);

    for record in &results {
        let left_payload = u64::from_le_bytes(record[8..16].try_into().unwrap());
        let right_payload = u64::from_le_bytes(record[80..88].try_into().unwrap());
        assert_eq!(left_payload, right_payload);
    }
}
