use phoenix_core::storage::{ExternalSort, SortConfig};

fn key_fn(record: &[u8; 16]) -> u64 {
    u64::from_le_bytes(record[0..8].try_into().unwrap())
}

fn make_record(key: u64, payload: u8) -> [u8; 16] {
    let mut rec = [payload; 16];
    rec[0..8].copy_from_slice(&key.to_le_bytes());
    rec
}

#[test]
fn test_sort_empty_input() {
    let records: Vec<[u8; 16]> = vec![];
    let sorted = ExternalSort::<16>::sort(records.into_iter(), key_fn, SortConfig::default()).unwrap();
    let result: Vec<_> = sorted.map(|r| r.unwrap()).collect();
    assert!(result.is_empty());
}

#[test]
fn test_sort_single_record() {
    let records = vec![make_record(42, 0xAA)];
    let sorted = ExternalSort::<16>::sort(records.into_iter(), key_fn, SortConfig::default()).unwrap();
    let result: Vec<_> = sorted.map(|r| r.unwrap()).collect();
    assert_eq!(result.len(), 1);
    assert_eq!(key_fn(&result[0]), 42);
    assert_eq!(result[0][8], 0xAA);
}

#[test]
fn test_sort_fits_in_memory() {
    let records: Vec<[u8; 16]> = (0..100u64)
        .rev()
        .map(|i| make_record(i, (i & 0xFF) as u8))
        .collect();

    let sorted = ExternalSort::<16>::sort(records.clone().into_iter(), key_fn, SortConfig::default()).unwrap();
    let result: Vec<_> = sorted.map(|r| r.unwrap()).collect();

    assert_eq!(result.len(), 100);
    for (i, rec) in result.iter().enumerate() {
        assert_eq!(key_fn(rec), i as u64);
    }
}

#[test]
fn test_sort_multiple_runs() {
    let config = SortConfig {
        buffer_pages: 1,
        max_k: 16,
    };
    let rpp = (4096 - 2) / 16;

    let total = rpp * 4;
    let records: Vec<[u8; 16]> = (0..total as u64)
        .rev()
        .map(|i| make_record(i, (i & 0xFF) as u8))
        .collect();

    let sorted = ExternalSort::<16>::sort(records.into_iter(), key_fn, config).unwrap();
    let result: Vec<_> = sorted.map(|r| r.unwrap()).collect();

    assert_eq!(result.len(), total);
    for (i, rec) in result.iter().enumerate() {
        assert_eq!(key_fn(rec), i as u64);
    }
}

#[test]
fn test_sort_multi_pass() {
    let config = SortConfig {
        buffer_pages: 1,
        max_k: 2,
    };
    let rpp = (4096 - 2) / 16;

    let total = rpp * 8;
    let records: Vec<[u8; 16]> = (0..total as u64)
        .rev()
        .map(|i| make_record(i, (i & 0xFF) as u8))
        .collect();

    let sorted = ExternalSort::<16>::sort(records.into_iter(), key_fn, config).unwrap();
    let result: Vec<_> = sorted.map(|r| r.unwrap()).collect();

    assert_eq!(result.len(), total);
    for (i, rec) in result.iter().enumerate() {
        assert_eq!(key_fn(rec), i as u64);
    }
}

#[test]
fn test_sort_already_sorted() {
    let records: Vec<[u8; 16]> = (0..500u64)
        .map(|i| make_record(i, (i & 0xFF) as u8))
        .collect();

    let sorted = ExternalSort::<16>::sort(records.into_iter(), key_fn, SortConfig::default()).unwrap();
    let result: Vec<_> = sorted.map(|r| r.unwrap()).collect();

    assert_eq!(result.len(), 500);
    for (i, rec) in result.iter().enumerate() {
        assert_eq!(key_fn(rec), i as u64);
    }
}

#[test]
fn test_sort_reverse_sorted() {
    let records: Vec<[u8; 16]> = (0..1000u64)
        .rev()
        .map(|i| make_record(i, (i & 0xFF) as u8))
        .collect();

    let sorted = ExternalSort::<16>::sort(records.into_iter(), key_fn, SortConfig::default()).unwrap();
    let result: Vec<_> = sorted.map(|r| r.unwrap()).collect();

    assert_eq!(result.len(), 1000);
    for (i, rec) in result.iter().enumerate() {
        assert_eq!(key_fn(rec), i as u64);
    }
}

#[test]
fn test_sort_duplicates() {
    let records: Vec<[u8; 16]> = (0..200u64)
        .map(|i| make_record(i / 4, (i & 0xFF) as u8))
        .collect();

    let sorted = ExternalSort::<16>::sort(records.into_iter(), key_fn, SortConfig::default()).unwrap();
    let result: Vec<_> = sorted.map(|r| r.unwrap()).collect();

    assert_eq!(result.len(), 200);
    for i in 1..result.len() {
        assert!(key_fn(&result[i]) >= key_fn(&result[i - 1]));
    }
}

#[test]
fn test_sort_large_dataset() {
    let config = SortConfig {
        buffer_pages: 4,
        max_k: 8,
    };

    let mut seed: u64 = 98765;
    let total = 100_000usize;
    let records: Vec<[u8; 16]> = (0..total)
        .map(|_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let key = seed;
            make_record(key, (key & 0xFF) as u8)
        })
        .collect();

    let sorted = ExternalSort::<16>::sort(records.into_iter(), key_fn, config).unwrap();
    let result: Vec<_> = sorted.map(|r| r.unwrap()).collect();

    assert_eq!(result.len(), total);
    for i in 1..result.len() {
        assert!(
            key_fn(&result[i]) >= key_fn(&result[i - 1]),
            "Not sorted at index {}: {} >= {}",
            i,
            key_fn(&result[i]),
            key_fn(&result[i - 1])
        );
    }
}

#[test]
fn test_sort_then_bulk_load_btree() {
    use phoenix_core::paging::{BufferPoolManager, ClockStrategy, DiskManager};
    use phoenix_core::storage::BPlusTree;
    use tempfile::NamedTempFile;

    let records: Vec<[u8; 72]> = (0..5000u64)
        .rev()
        .map(|i| {
            let mut rec = [0u8; 72];
            rec[0..8].copy_from_slice(&i.to_le_bytes());
            rec[8..16].copy_from_slice(&(i * 2).to_le_bytes());
            rec
        })
        .collect();

    let sorted = ExternalSort::<72>::sort(
        records.into_iter(),
        |rec| u64::from_le_bytes(rec[0..8].try_into().unwrap()),
        SortConfig::default(),
    )
    .unwrap();

    let tmp = NamedTempFile::new().unwrap();
    let dm = DiskManager::new(tmp.path()).unwrap();
    let bpm = Box::leak(Box::new(BufferPoolManager::new(100, dm, ClockStrategy::new(100))));
    let tree = BPlusTree::<ClockStrategy, 64>::create(bpm).unwrap();

    for record in sorted {
        let record = record.unwrap();
        let key = u64::from_le_bytes(record[0..8].try_into().unwrap());
        let value: [u8; 64] = record[8..72].try_into().unwrap();
        tree.insert(key, &value).unwrap();
    }

    for i in 0..5000u64 {
        let result = tree.search(i).unwrap();
        let stored = u64::from_le_bytes(result[0..8].try_into().unwrap());
        assert_eq!(stored, i * 2);
    }
}
