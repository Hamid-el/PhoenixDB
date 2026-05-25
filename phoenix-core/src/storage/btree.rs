use std::sync::atomic::{AtomicU32, Ordering};

use crate::paging::buffer_pool::BufferPoolManager;
use crate::paging::constants::INVALID_PAGE_ID;
use crate::paging::error::{DbError, Result};
use crate::paging::replacement::ReplacementStrategy;
use crate::paging::types::PageId;
use crate::storage::node::{
    InternalNode, InternalNodeMut, LeafNode, LeafNodeMut, NodeType, read_node_type,
};

pub struct BPlusTree<'a, R: ReplacementStrategy, const VALUE_SIZE: usize> {
    bpm: &'a BufferPoolManager<R>,
    root_page_id: AtomicU32,
}

impl<'a, R: ReplacementStrategy, const VALUE_SIZE: usize> BPlusTree<'a, R, VALUE_SIZE> {
    pub fn create(bpm: &'a BufferPoolManager<R>) -> Result<Self> {
        let handle = bpm.new_page()?;
        let root_id = handle.page_id();
        {
            let mut data = handle.write();
            let mut leaf = LeafNodeMut::<VALUE_SIZE>::new(&mut data);
            leaf.init();
        }
        bpm.unpin_page(root_id, true)?;

        Ok(Self {
            bpm,
            root_page_id: AtomicU32::new(root_id),
        })
    }

    pub fn open(bpm: &'a BufferPoolManager<R>, root_page_id: PageId) -> Self {
        Self {
            bpm,
            root_page_id: AtomicU32::new(root_page_id),
        }
    }

    pub fn root_page_id(&self) -> PageId {
        self.root_page_id.load(Ordering::SeqCst)
    }

    pub fn search(&self, key: u64) -> Result<[u8; VALUE_SIZE]> {
        let leaf_page_id = self.find_leaf(key)?;
        let handle = self.bpm.fetch_page(leaf_page_id)?;
        let data = handle.read();
        let leaf = LeafNode::<VALUE_SIZE>::new(&data);

        match leaf.find_key(key) {
            Ok(idx) => {
                let mut value = [0u8; VALUE_SIZE];
                value.copy_from_slice(leaf.value_at(idx));
                drop(data);
                self.bpm.unpin_page(leaf_page_id, false)?;
                Ok(value)
            }
            Err(_) => {
                drop(data);
                self.bpm.unpin_page(leaf_page_id, false)?;
                Err(DbError::KeyNotFound { key })
            }
        }
    }

    pub fn range_scan(&self, start: u64, end: u64) -> Result<Vec<(u64, [u8; VALUE_SIZE])>> {
        let mut results = Vec::new();
        let mut page_id = self.find_leaf(start)?;

        loop {
            let handle = self.bpm.fetch_page(page_id)?;
            let data = handle.read();
            let leaf = LeafNode::<VALUE_SIZE>::new(&data);

            let n = leaf.num_keys() as usize;
            let start_idx = match leaf.find_key(start) {
                Ok(i) => i,
                Err(i) => i,
            };

            for i in start_idx..n {
                let k = leaf.key_at(i);
                if k > end {
                    drop(data);
                    self.bpm.unpin_page(page_id, false)?;
                    return Ok(results);
                }
                let mut value = [0u8; VALUE_SIZE];
                value.copy_from_slice(leaf.value_at(i));
                results.push((k, value));
            }

            let next = leaf.next_leaf_id();
            drop(data);
            self.bpm.unpin_page(page_id, false)?;

            if next == INVALID_PAGE_ID {
                break;
            }
            page_id = next;
        }

        Ok(results)
    }

    pub fn insert(&self, key: u64, value: &[u8; VALUE_SIZE]) -> Result<()> {
        let leaf_page_id = self.find_leaf(key)?;
        let handle = self.bpm.fetch_page(leaf_page_id)?;

        {
            let data = handle.read();
            let leaf = LeafNode::<VALUE_SIZE>::new(&data);
            if leaf.find_key(key).is_ok() {
                drop(data);
                self.bpm.unpin_page(leaf_page_id, false)?;
                return Err(DbError::DuplicateKey { key });
            }
        }

        let mut data = handle.write();
        let mut leaf = LeafNodeMut::<VALUE_SIZE>::new(&mut data);
        let n = leaf.num_keys() as usize;

        if n < LeafNodeMut::<VALUE_SIZE>::max_keys() {
            let pos = match leaf.find_key(key) {
                Ok(_) => unreachable!(),
                Err(i) => i,
            };
            leaf.insert_at(pos, key, value);
            drop(data);
            self.bpm.unpin_page(leaf_page_id, true)?;
            return Ok(());
        }

        drop(data);
        self.bpm.unpin_page(leaf_page_id, true)?;
        self.split_leaf_and_insert(leaf_page_id, key, value)
    }

    fn find_leaf(&self, key: u64) -> Result<PageId> {
        let mut page_id = self.root_page_id.load(Ordering::SeqCst);

        loop {
            let handle = self.bpm.fetch_page(page_id)?;
            let data = handle.read();

            match read_node_type(&data) {
                Some(NodeType::Leaf) => {
                    drop(data);
                    self.bpm.unpin_page(page_id, false)?;
                    return Ok(page_id);
                }
                Some(NodeType::Internal) => {
                    let node = InternalNode::new(&data);
                    let child = node.find_child(key);
                    drop(data);
                    self.bpm.unpin_page(page_id, false)?;
                    page_id = child;
                }
                None => {
                    drop(data);
                    self.bpm.unpin_page(page_id, false)?;
                    return Err(DbError::CorruptedNode {
                        page_id,
                        reason: "invalid node type byte".to_string(),
                    });
                }
            }
        }
    }

    fn split_leaf_and_insert(
        &self,
        leaf_page_id: PageId,
        key: u64,
        value: &[u8; VALUE_SIZE],
    ) -> Result<()> {
        let new_handle = self.bpm.new_page()?;
        let new_page_id = new_handle.page_id();
        {
            let mut new_data = new_handle.write();
            let mut new_leaf = LeafNodeMut::<VALUE_SIZE>::new(&mut new_data);
            new_leaf.init();
        }
        self.bpm.unpin_page(new_page_id, true)?;

        let handle = self.bpm.fetch_page(leaf_page_id)?;
        let new_handle = self.bpm.fetch_page(new_page_id)?;

        let push_up_key;
        let parent_id;
        let old_next;
        {
            let mut left_data = handle.write();
            let mut right_data = new_handle.write();
            let mut left_leaf = LeafNodeMut::<VALUE_SIZE>::new(&mut left_data);
            let mut right_leaf = LeafNodeMut::<VALUE_SIZE>::new(&mut right_data);

            let split_key = left_leaf.split_into(&mut right_leaf);

            if key < split_key {
                let pos = match left_leaf.find_key(key) {
                    Ok(_) => unreachable!(),
                    Err(i) => i,
                };
                left_leaf.insert_at(pos, key, value);
            } else {
                let pos = match right_leaf.find_key(key) {
                    Ok(_) => unreachable!(),
                    Err(i) => i,
                };
                right_leaf.insert_at(pos, key, value);
            }

            push_up_key = right_leaf.key_at(0);

            old_next = left_leaf.next_leaf_id();
            right_leaf.set_next_leaf_id(old_next);
            right_leaf.set_prev_leaf_id(leaf_page_id);
            left_leaf.set_next_leaf_id(new_page_id);

            parent_id = left_leaf.parent_page_id();
        }

        self.bpm.unpin_page(leaf_page_id, true)?;
        self.bpm.unpin_page(new_page_id, true)?;

        if old_next != INVALID_PAGE_ID {
            let next_handle = self.bpm.fetch_page(old_next)?;
            let mut next_data = next_handle.write();
            let mut next_leaf = LeafNodeMut::<VALUE_SIZE>::new(&mut next_data);
            next_leaf.set_prev_leaf_id(new_page_id);
            drop(next_data);
            self.bpm.unpin_page(old_next, true)?;
        }

        self.insert_in_parent(leaf_page_id, push_up_key, new_page_id, parent_id)
    }

    fn insert_in_parent(
        &self,
        left_page_id: PageId,
        key: u64,
        right_page_id: PageId,
        parent_page_id: PageId,
    ) -> Result<()> {
        if parent_page_id == INVALID_PAGE_ID {
            let new_root_handle = self.bpm.new_page()?;
            let new_root_id = new_root_handle.page_id();
            {
                let mut data = new_root_handle.write();
                let mut node = InternalNodeMut::new(&mut data);
                node.init();
                node.insert_at(0, key, left_page_id);
                node.set_rightmost_child(right_page_id);
            }
            self.bpm.unpin_page(new_root_id, true)?;

            self.set_parent(left_page_id, new_root_id)?;
            self.set_parent(right_page_id, new_root_id)?;

            self.root_page_id.store(new_root_id, Ordering::SeqCst);
            return Ok(());
        }

        let parent_handle = self.bpm.fetch_page(parent_page_id)?;
        let mut parent_data = parent_handle.write();
        let mut parent_node = InternalNodeMut::new(&mut parent_data);

        let n = parent_node.num_keys() as usize;
        if n < InternalNodeMut::max_keys() {
            let pos = {
                let mut i = 0;
                while i < n && parent_node.key_at(i) < key {
                    i += 1;
                }
                i
            };
            parent_node.insert_at(pos, key, left_page_id);
            let new_n = parent_node.num_keys() as usize;
            if pos + 1 < new_n {
                parent_node.set_child_at(pos + 1, right_page_id);
            } else {
                parent_node.set_rightmost_child(right_page_id);
            }

            drop(parent_data);
            self.bpm.unpin_page(parent_page_id, true)?;

            self.set_parent(right_page_id, parent_page_id)?;
            return Ok(());
        }

        drop(parent_data);
        self.bpm.unpin_page(parent_page_id, false)?;
        self.split_internal_and_insert(parent_page_id, key, left_page_id, right_page_id)
    }

    fn split_internal_and_insert(
        &self,
        internal_page_id: PageId,
        key: u64,
        left_child: PageId,
        right_child: PageId,
    ) -> Result<()> {
        let new_handle = self.bpm.new_page()?;
        let new_page_id = new_handle.page_id();
        {
            let mut new_data = new_handle.write();
            let mut new_node = InternalNodeMut::new(&mut new_data);
            new_node.init();
        }
        self.bpm.unpin_page(new_page_id, true)?;

        let handle = self.bpm.fetch_page(internal_page_id)?;
        let new_handle = self.bpm.fetch_page(new_page_id)?;

        let push_up_key;
        let grandparent_id;
        {
            let mut left_data = handle.write();
            let mut right_data = new_handle.write();
            let mut left_node = InternalNodeMut::new(&mut left_data);
            let mut right_node = InternalNodeMut::new(&mut right_data);

            push_up_key = left_node.split_into(&mut right_node);
            grandparent_id = left_node.parent_page_id();

            if key < push_up_key {
                let n = left_node.num_keys() as usize;
                let pos = {
                    let mut i = 0;
                    while i < n && left_node.key_at(i) < key {
                        i += 1;
                    }
                    i
                };
                left_node.insert_at(pos, key, left_child);
                let new_n = left_node.num_keys() as usize;
                if pos + 1 < new_n {
                    left_node.set_child_at(pos + 1, right_child);
                } else {
                    left_node.set_rightmost_child(right_child);
                }
            } else {
                let n = right_node.num_keys() as usize;
                let pos = {
                    let mut i = 0;
                    while i < n && right_node.key_at(i) < key {
                        i += 1;
                    }
                    i
                };
                right_node.insert_at(pos, key, left_child);
                let new_n = right_node.num_keys() as usize;
                if pos + 1 < new_n {
                    right_node.set_child_at(pos + 1, right_child);
                } else {
                    right_node.set_rightmost_child(right_child);
                }
            }
        }

        self.bpm.unpin_page(internal_page_id, true)?;
        self.bpm.unpin_page(new_page_id, true)?;

        self.update_children_parent(new_page_id)?;

        self.insert_in_parent(internal_page_id, push_up_key, new_page_id, grandparent_id)
    }

    fn set_parent(&self, child_page_id: PageId, parent_page_id: PageId) -> Result<()> {
        let handle = self.bpm.fetch_page(child_page_id)?;
        let mut data = handle.write();
        data[3..7].copy_from_slice(&parent_page_id.to_le_bytes());
        drop(data);
        self.bpm.unpin_page(child_page_id, true)?;
        Ok(())
    }

    fn update_children_parent(&self, internal_page_id: PageId) -> Result<()> {
        let handle = self.bpm.fetch_page(internal_page_id)?;
        let data = handle.read();
        let node = InternalNode::new(&data);

        let n = node.num_keys() as usize;
        let mut children = Vec::with_capacity(n + 1);
        for i in 0..n {
            children.push(node.child_at(i));
        }
        children.push(node.rightmost_child());
        drop(data);
        self.bpm.unpin_page(internal_page_id, false)?;

        for child_id in children {
            if child_id != INVALID_PAGE_ID {
                self.set_parent(child_id, internal_page_id)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paging::replacement::ClockStrategy;
    use crate::paging::disk_manager::DiskManager;
    use tempfile::NamedTempFile;

    fn create_tree() -> (BPlusTree<'static, ClockStrategy, 64>, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let dm = DiskManager::new(tmp.path()).unwrap();
        let bpm = Box::leak(Box::new(BufferPoolManager::new(100, dm, ClockStrategy::new(100))));
        let tree = BPlusTree::<ClockStrategy, 64>::create(bpm).unwrap();
        (tree, tmp)
    }

    #[test]
    fn test_create_empty_tree() {
        let (tree, _tmp) = create_tree();
        let root = tree.root_page_id();
        assert_ne!(root, INVALID_PAGE_ID);
    }

    #[test]
    fn test_insert_single_key() {
        let (tree, _tmp) = create_tree();
        let val = [0xABu8; 64];
        tree.insert(42, &val).unwrap();

        let result = tree.search(42).unwrap();
        assert_eq!(result[0], 0xAB);
    }

    #[test]
    fn test_insert_multiple_keys_sorted() {
        let (tree, _tmp) = create_tree();
        for i in 0..10u64 {
            let mut val = [0u8; 64];
            val[0] = i as u8;
            tree.insert(i * 10, &val).unwrap();
        }

        for i in 0..10u64 {
            let result = tree.search(i * 10).unwrap();
            assert_eq!(result[0], i as u8);
        }
    }

    #[test]
    fn test_search_nonexistent_key() {
        let (tree, _tmp) = create_tree();
        let val = [0u8; 64];
        tree.insert(10, &val).unwrap();

        let result = tree.search(99);
        assert!(matches!(result, Err(DbError::KeyNotFound { key: 99 })));
    }

    #[test]
    fn test_duplicate_key_error() {
        let (tree, _tmp) = create_tree();
        let val = [0u8; 64];
        tree.insert(42, &val).unwrap();

        let result = tree.insert(42, &val);
        assert!(matches!(result, Err(DbError::DuplicateKey { key: 42 })));
    }

    #[test]
    fn test_leaf_split() {
        let (tree, _tmp) = create_tree();
        let max = LeafNodeMut::<64>::max_keys();

        for i in 0..=(max as u64) {
            let mut val = [0u8; 64];
            val[0] = (i & 0xFF) as u8;
            tree.insert(i, &val).unwrap();
        }

        for i in 0..=(max as u64) {
            let result = tree.search(i).unwrap();
            assert_eq!(result[0], (i & 0xFF) as u8);
        }
    }

    #[test]
    fn test_range_scan_basic() {
        let (tree, _tmp) = create_tree();
        for i in 0..20u64 {
            let mut val = [0u8; 64];
            val[0] = i as u8;
            tree.insert(i, &val).unwrap();
        }

        let results = tree.range_scan(5, 14).unwrap();
        assert_eq!(results.len(), 10);
        for (idx, (k, v)) in results.iter().enumerate() {
            assert_eq!(*k, (idx + 5) as u64);
            assert_eq!(v[0], (idx + 5) as u8);
        }
    }

    #[test]
    fn test_root_grows_height() {
        let (tree, _tmp) = create_tree();
        let max = LeafNodeMut::<64>::max_keys();

        for i in 0..((max as u64 + 1) * 3) {
            let mut val = [0u8; 64];
            val[0] = (i & 0xFF) as u8;
            tree.insert(i, &val).unwrap();
        }

        for i in 0..((max as u64 + 1) * 3) {
            let result = tree.search(i).unwrap();
            assert_eq!(result[0], (i & 0xFF) as u8);
        }
    }

    #[test]
    fn test_range_scan_across_leaves() {
        let (tree, _tmp) = create_tree();
        let max = LeafNodeMut::<64>::max_keys();
        let total = (max as u64 + 1) * 2;

        for i in 0..total {
            let mut val = [0u8; 64];
            val[0] = (i & 0xFF) as u8;
            tree.insert(i, &val).unwrap();
        }

        let results = tree.range_scan(0, total - 1).unwrap();
        assert_eq!(results.len(), total as usize);
    }
}
