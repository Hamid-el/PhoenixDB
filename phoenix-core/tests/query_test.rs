use phoenix_core::paging::{BufferPoolManager, ClockStrategy, DiskManager};
use phoenix_core::query::join::EquiHashJoin;
use phoenix_core::query::scan::TableScan;
use phoenix_core::query::sort::Sort;
use phoenix_core::query::types::{Record, Value};
use phoenix_core::query::Operator;
use phoenix_core::storage::BPlusTree;
use tempfile::NamedTempFile;

fn decode_record(raw: &[u8]) -> Record {
    let key = u64::from_le_bytes(raw[0..8].try_into().unwrap()) as i32;
    let payload = u64::from_le_bytes(raw[8..16].try_into().unwrap()) as i32;
    Record::new(vec![Value::Int(key), Value::Int(payload)])
}

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

#[test]
fn test_table_scan_full() {
    let (bpm, root) = setup_tree(100);

    let mut scan = TableScan::<ClockStrategy, 64>::new(bpm, root, decode_record);
    scan.open().unwrap();

    let mut count = 0i32;
    let mut prev_key = -1i32;
    while let Some(record) = scan.next().unwrap() {
        let key = match record.get(0).unwrap() {
            Value::Int(k) => *k,
            _ => panic!("expected Int"),
        };
        assert!(key > prev_key, "keys must be in ascending order");
        prev_key = key;
        count += 1;
    }
    assert_eq!(count, 100);
    scan.close().unwrap();
}

#[test]
fn test_table_scan_range() {
    let (bpm, root) = setup_tree(1000);

    let mut scan = TableScan::<ClockStrategy, 64>::range(bpm, root, 100, 200, decode_record);
    scan.open().unwrap();

    let mut count = 0;
    while let Some(record) = scan.next().unwrap() {
        let key = match record.get(0).unwrap() {
            Value::Int(k) => *k,
            _ => panic!("expected Int"),
        };
        assert!(key >= 100 && key <= 200);
        count += 1;
    }
    assert_eq!(count, 101);
    scan.close().unwrap();
}

#[test]
fn test_table_scan_empty() {
    let (bpm, root) = setup_tree(0);

    let mut scan = TableScan::<ClockStrategy, 64>::new(bpm, root, decode_record);
    scan.open().unwrap();
    assert!(scan.next().unwrap().is_none());
    scan.close().unwrap();
}

#[test]
fn test_table_scan_payload() {
    let (bpm, root) = setup_tree(1000);

    let mut scan = TableScan::<ClockStrategy, 64>::new(bpm, root, decode_record);
    scan.open().unwrap();

    let mut results = Vec::new();
    while let Some(record) = scan.next().unwrap() {
        results.push(record);
    }
    scan.close().unwrap();

    assert_eq!(results.len(), 1000);
    for (i, record) in results.iter().enumerate() {
        let key = match record.get(0).unwrap() {
            Value::Int(k) => *k,
            _ => panic!("expected Int"),
        };
        let payload = match record.get(1).unwrap() {
            Value::Int(p) => *p,
            _ => panic!("expected Int"),
        };
        assert_eq!(key, i as i32);
        assert_eq!(payload, i as i32 * 10);
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

    let scan = TableScan::<ClockStrategy, 64>::new(bpm, root, decode_record);

    fn sort_by_payload(a: &Record, b: &Record) -> std::cmp::Ordering {
        let a_val = match a.get(1).unwrap() {
            Value::Int(v) => *v,
            _ => 0,
        };
        let b_val = match b.get(1).unwrap() {
            Value::Int(v) => *v,
            _ => 0,
        };
        a_val.cmp(&b_val)
    }

    let mut sort_op = Sort::new(Box::new(scan), sort_by_payload);
    sort_op.open().unwrap();

    let mut prev: Option<i32> = None;
    let mut count = 0;
    while let Some(record) = sort_op.next().unwrap() {
        let payload = match record.get(1).unwrap() {
            Value::Int(v) => *v,
            _ => panic!("expected Int"),
        };
        if let Some(p) = prev {
            assert!(payload >= p, "sort output not in order");
        }
        prev = Some(payload);
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

    let left_scan = TableScan::<ClockStrategy, 64>::new(bpm_left, root_left, decode_record);
    let right_scan = TableScan::<ClockStrategy, 64>::new(bpm_right, root_right, decode_record);

    // Join on key column (index 0)
    let mut join = EquiHashJoin::new(
        Box::new(left_scan),
        Box::new(right_scan),
        0,
        0,
    );
    join.open().unwrap();

    let mut results = Vec::new();
    while let Some(record) = join.next().unwrap() {
        assert_eq!(record.len(), 4); // left(2 fields) + right(2 fields)
        results.push(record);
    }
    join.close().unwrap();

    assert_eq!(results.len(), 50);
    for record in &results {
        let left_key = record.get(0).unwrap();
        let right_key = record.get(2).unwrap();
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

    let left_scan = TableScan::<ClockStrategy, 64>::new(bpm_left, root_left, decode_record);
    let right_scan = TableScan::<ClockStrategy, 64>::new(bpm_right, root_right, decode_record);

    let mut join = EquiHashJoin::new(
        Box::new(left_scan),
        Box::new(right_scan),
        0,
        0,
    );
    join.open().unwrap();
    assert!(join.next().unwrap().is_none());
    join.close().unwrap();
}

#[test]
fn test_hash_join_many_to_many() {
    let tmp_left = NamedTempFile::new().unwrap();
    let dm_left = DiskManager::new(tmp_left.path()).unwrap();
    let bpm_left = Box::leak(Box::new(BufferPoolManager::new(
        100,
        dm_left,
        ClockStrategy::new(100),
    )));
    let tree_left = BPlusTree::<ClockStrategy, 64>::create(bpm_left).unwrap();
    let root_left = tree_left.root_page_id();

    // 20 records with payload = key % 5 (so 4 records per group)
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

    // 10 records with payload = key % 5 (so 2 records per group)
    for i in 0..10u64 {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&(i % 5).to_le_bytes());
        tree_right.insert(i, &value).unwrap();
    }

    let left_scan = TableScan::<ClockStrategy, 64>::new(bpm_left, root_left, decode_record);
    let right_scan = TableScan::<ClockStrategy, 64>::new(bpm_right, root_right, decode_record);

    // Join on payload column (index 1)
    let mut join = EquiHashJoin::new(
        Box::new(left_scan),
        Box::new(right_scan),
        1,
        1,
    );
    join.open().unwrap();

    let mut count = 0;
    while join.next().unwrap().is_some() {
        count += 1;
    }
    join.close().unwrap();

    // Each of the 5 groups: 4 left * 2 right = 8 matches, total = 40
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

    // Insert 100 records: key=i, payload=100-i (descending payloads)
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

    // Insert 50 records: key=i (1..51), payload=i
    for i in 1..51u64 {
        let mut value = [0u8; 64];
        value[0..8].copy_from_slice(&i.to_le_bytes());
        tree_right.insert(i, &value).unwrap();
    }

    // Pipeline: left_scan -> sort(by payload) -> join(on payload = right payload)
    let left_scan = TableScan::<ClockStrategy, 64>::new(bpm_left, root_left, decode_record);

    fn sort_by_payload(a: &Record, b: &Record) -> std::cmp::Ordering {
        a.get(1).cmp(&b.get(1))
    }

    let sorted_left = Sort::new(Box::new(left_scan), sort_by_payload);
    let right_scan = TableScan::<ClockStrategy, 64>::new(bpm_right, root_right, decode_record);

    // Join on payload column (index 1)
    let mut join = EquiHashJoin::new(
        Box::new(sorted_left),
        Box::new(right_scan),
        1,
        1,
    );
    join.open().unwrap();

    let mut results = Vec::new();
    while let Some(record) = join.next().unwrap() {
        results.push(record);
    }
    join.close().unwrap();

    assert_eq!(results.len(), 50);
    for record in &results {
        let left_payload = record.get(1).unwrap();
        let right_payload = record.get(3).unwrap();
        assert_eq!(left_payload, right_payload);
    }
}
