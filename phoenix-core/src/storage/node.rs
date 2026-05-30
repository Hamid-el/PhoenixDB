use crate::paging::constants::PAGE_SIZE;
use crate::paging::types::PageId;
use crate::paging::constants::INVALID_PAGE_ID;

const INTERNAL_HEADER_SIZE: usize = 11;
const LEAF_HEADER_SIZE: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum NodeType {
    Internal = 1,
    Leaf = 2,
}

pub fn read_node_type(data: &[u8; PAGE_SIZE]) -> Option<NodeType> {
    match data[0] {
        1 => Some(NodeType::Internal),
        2 => Some(NodeType::Leaf),
        _ => None,
    }
}

pub struct InternalNode<'a> {
    data: &'a [u8; PAGE_SIZE],
}

pub struct InternalNodeMut<'a> {
    data: &'a mut [u8; PAGE_SIZE],
}

pub struct LeafNode<'a, const VALUE_SIZE: usize> {
    data: &'a [u8; PAGE_SIZE],
}

pub struct LeafNodeMut<'a, const VALUE_SIZE: usize> {
    data: &'a mut [u8; PAGE_SIZE],
}

fn max_internal_keys() -> usize {
    (PAGE_SIZE - INTERNAL_HEADER_SIZE) / 12
}

fn max_leaf_keys<const VALUE_SIZE: usize>() -> usize {
    (PAGE_SIZE - LEAF_HEADER_SIZE) / (8 + VALUE_SIZE)
}

impl<'a> InternalNode<'a> {
    pub fn new(data: &'a [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn num_keys(&self) -> u16 {
        u16::from_le_bytes([self.data[1], self.data[2]])
    }

    pub fn parent_page_id(&self) -> PageId {
        u32::from_le_bytes([self.data[3], self.data[4], self.data[5], self.data[6]])
    }

    pub fn rightmost_child(&self) -> PageId {
        u32::from_le_bytes([self.data[7], self.data[8], self.data[9], self.data[10]])
    }

    pub fn key_at(&self, index: usize) -> u64 {
        let offset = INTERNAL_HEADER_SIZE + index * 12;
        u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap())
    }

    pub fn child_at(&self, index: usize) -> PageId {
        let offset = INTERNAL_HEADER_SIZE + index * 12 + 8;
        u32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap())
    }

    pub fn find_child(&self, key: u64) -> PageId {
        let n = self.num_keys() as usize;
        for i in 0..n {
            if key < self.key_at(i) {
                return self.child_at(i);
            }
        }
        self.rightmost_child()
    }
}

impl<'a> InternalNodeMut<'a> {
    pub fn new(data: &'a mut [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn init(&mut self) {
        self.data[..INTERNAL_HEADER_SIZE].fill(0);
        self.data[0] = NodeType::Internal as u8;
        self.set_parent_page_id(INVALID_PAGE_ID);
        self.set_rightmost_child(INVALID_PAGE_ID);
    }

    pub fn num_keys(&self) -> u16 {
        u16::from_le_bytes([self.data[1], self.data[2]])
    }

    pub fn set_num_keys(&mut self, n: u16) {
        let bytes = n.to_le_bytes();
        self.data[1] = bytes[0];
        self.data[2] = bytes[1];
    }

    pub fn parent_page_id(&self) -> PageId {
        u32::from_le_bytes([self.data[3], self.data[4], self.data[5], self.data[6]])
    }

    pub fn set_parent_page_id(&mut self, page_id: PageId) {
        self.data[3..7].copy_from_slice(&page_id.to_le_bytes());
    }

    pub fn rightmost_child(&self) -> PageId {
        u32::from_le_bytes([self.data[7], self.data[8], self.data[9], self.data[10]])
    }

    pub fn set_rightmost_child(&mut self, page_id: PageId) {
        self.data[7..11].copy_from_slice(&page_id.to_le_bytes());
    }

    pub fn key_at(&self, index: usize) -> u64 {
        let offset = INTERNAL_HEADER_SIZE + index * 12;
        u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap())
    }

    pub fn set_key_at(&mut self, index: usize, key: u64) {
        let offset = INTERNAL_HEADER_SIZE + index * 12;
        self.data[offset..offset + 8].copy_from_slice(&key.to_le_bytes());
    }

    pub fn child_at(&self, index: usize) -> PageId {
        let offset = INTERNAL_HEADER_SIZE + index * 12 + 8;
        u32::from_le_bytes(self.data[offset..offset + 4].try_into().unwrap())
    }

    pub fn set_child_at(&mut self, index: usize, page_id: PageId) {
        let offset = INTERNAL_HEADER_SIZE + index * 12 + 8;
        self.data[offset..offset + 4].copy_from_slice(&page_id.to_le_bytes());
    }

    pub fn insert_at(&mut self, index: usize, key: u64, child: PageId) {
        let n = self.num_keys() as usize;
        let entry_size = 12;
        let start = INTERNAL_HEADER_SIZE + index * entry_size;
        let end = INTERNAL_HEADER_SIZE + n * entry_size;
        self.data.copy_within(start..end, start + entry_size);
        self.set_key_at(index, key);
        self.set_child_at(index, child);
        self.set_num_keys((n + 1) as u16);
    }

    pub fn split_into(&mut self, right: &mut InternalNodeMut) -> u64 {
        let n = self.num_keys() as usize;
        let mid = n / 2;
        let push_up_key = self.key_at(mid);

        let right_start = mid + 1;
        let right_count = n - right_start;

        for i in 0..right_count {
            let src_idx = right_start + i;
            right.set_key_at(i, self.key_at(src_idx));
            right.set_child_at(i, self.child_at(src_idx));
        }
        right.set_rightmost_child(self.rightmost_child());
        right.set_num_keys(right_count as u16);

        self.set_rightmost_child(self.child_at(mid));
        self.set_num_keys(mid as u16);

        push_up_key
    }

    pub fn find_child(&self, key: u64) -> PageId {
        let n = self.num_keys() as usize;
        for i in 0..n {
            if key < self.key_at(i) {
                return self.child_at(i);
            }
        }
        self.rightmost_child()
    }

    pub fn max_keys() -> usize {
        max_internal_keys()
    }
}

impl<'a, const VALUE_SIZE: usize> LeafNode<'a, VALUE_SIZE> {
    pub fn new(data: &'a [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn num_keys(&self) -> u16 {
        u16::from_le_bytes([self.data[1], self.data[2]])
    }

    pub fn parent_page_id(&self) -> PageId {
        u32::from_le_bytes([self.data[3], self.data[4], self.data[5], self.data[6]])
    }

    pub fn next_leaf_id(&self) -> PageId {
        u32::from_le_bytes([self.data[7], self.data[8], self.data[9], self.data[10]])
    }

    pub fn prev_leaf_id(&self) -> PageId {
        u32::from_le_bytes([self.data[11], self.data[12], self.data[13], self.data[14]])
    }

    pub fn key_at(&self, index: usize) -> u64 {
        let entry_size = 8 + VALUE_SIZE;
        let offset = LEAF_HEADER_SIZE + index * entry_size;
        u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap())
    }

    pub fn value_at(&self, index: usize) -> &[u8] {
        let entry_size = 8 + VALUE_SIZE;
        let offset = LEAF_HEADER_SIZE + index * entry_size + 8;
        &self.data[offset..offset + VALUE_SIZE]
    }

    pub fn find_key(&self, key: u64) -> Result<usize, usize> {
        let n = self.num_keys() as usize;
        let mut lo = 0;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_key = self.key_at(mid);
            if mid_key == key {
                return Ok(mid);
            } else if mid_key < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Err(lo)
    }

    pub fn max_keys() -> usize {
        max_leaf_keys::<VALUE_SIZE>()
    }
}

impl<'a, const VALUE_SIZE: usize> LeafNodeMut<'a, VALUE_SIZE> {
    pub fn new(data: &'a mut [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    pub fn init(&mut self) {
        self.data[..LEAF_HEADER_SIZE].fill(0);
        self.data[0] = NodeType::Leaf as u8;
        self.set_parent_page_id(INVALID_PAGE_ID);
        self.set_next_leaf_id(INVALID_PAGE_ID);
        self.set_prev_leaf_id(INVALID_PAGE_ID);
    }

    pub fn num_keys(&self) -> u16 {
        u16::from_le_bytes([self.data[1], self.data[2]])
    }

    pub fn set_num_keys(&mut self, n: u16) {
        let bytes = n.to_le_bytes();
        self.data[1] = bytes[0];
        self.data[2] = bytes[1];
    }

    pub fn parent_page_id(&self) -> PageId {
        u32::from_le_bytes([self.data[3], self.data[4], self.data[5], self.data[6]])
    }

    pub fn set_parent_page_id(&mut self, page_id: PageId) {
        self.data[3..7].copy_from_slice(&page_id.to_le_bytes());
    }

    pub fn next_leaf_id(&self) -> PageId {
        u32::from_le_bytes([self.data[7], self.data[8], self.data[9], self.data[10]])
    }

    pub fn set_next_leaf_id(&mut self, page_id: PageId) {
        self.data[7..11].copy_from_slice(&page_id.to_le_bytes());
    }

    pub fn prev_leaf_id(&self) -> PageId {
        u32::from_le_bytes([self.data[11], self.data[12], self.data[13], self.data[14]])
    }

    pub fn set_prev_leaf_id(&mut self, page_id: PageId) {
        self.data[11..15].copy_from_slice(&page_id.to_le_bytes());
    }

    pub fn key_at(&self, index: usize) -> u64 {
        let entry_size = 8 + VALUE_SIZE;
        let offset = LEAF_HEADER_SIZE + index * entry_size;
        u64::from_le_bytes(self.data[offset..offset + 8].try_into().unwrap())
    }

    pub fn value_at(&self, index: usize) -> &[u8] {
        let entry_size = 8 + VALUE_SIZE;
        let offset = LEAF_HEADER_SIZE + index * entry_size + 8;
        &self.data[offset..offset + VALUE_SIZE]
    }

    pub fn find_key(&self, key: u64) -> Result<usize, usize> {
        let n = self.num_keys() as usize;
        let mut lo = 0;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let mid_key = self.key_at(mid);
            if mid_key == key {
                return Ok(mid);
            } else if mid_key < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        Err(lo)
    }

    pub fn insert_at(&mut self, index: usize, key: u64, value: &[u8; VALUE_SIZE]) {
        let n = self.num_keys() as usize;
        let entry_size = 8 + VALUE_SIZE;
        let start = LEAF_HEADER_SIZE + index * entry_size;
        let end = LEAF_HEADER_SIZE + n * entry_size;
        self.data.copy_within(start..end, start + entry_size);

        self.data[start..start + 8].copy_from_slice(&key.to_le_bytes());
        self.data[start + 8..start + 8 + VALUE_SIZE].copy_from_slice(value);
        self.set_num_keys((n + 1) as u16);
    }

    pub fn split_into(&mut self, right: &mut LeafNodeMut<VALUE_SIZE>) -> u64 {
        let n = self.num_keys() as usize;
        let split_point = n.div_ceil(2);
        let right_count = n - split_point;
        let entry_size = 8 + VALUE_SIZE;

        let src_start = LEAF_HEADER_SIZE + split_point * entry_size;
        let dst_start = LEAF_HEADER_SIZE;

        for i in 0..right_count {
            let s = src_start + i * entry_size;
            let d = dst_start + i * entry_size;
            right.data[d..d + entry_size].copy_from_slice(&self.data[s..s + entry_size]);
        }

        right.set_num_keys(right_count as u16);
        self.set_num_keys(split_point as u16);

        right.key_at(0)
    }

    pub fn max_keys() -> usize {
        max_leaf_keys::<VALUE_SIZE>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leaf_node_read_write_header() {
        let mut page = [0u8; PAGE_SIZE];
        {
            let mut leaf = LeafNodeMut::<64>::new(&mut page);
            leaf.init();
        }

        assert_eq!(page[0], NodeType::Leaf as u8);

        let mut leaf = LeafNodeMut::<64>::new(&mut page);
        assert_eq!(leaf.num_keys(), 0);
        assert_eq!(leaf.parent_page_id(), INVALID_PAGE_ID);
        assert_eq!(leaf.next_leaf_id(), INVALID_PAGE_ID);
        assert_eq!(leaf.prev_leaf_id(), INVALID_PAGE_ID);

        leaf.set_parent_page_id(42);
        assert_eq!(leaf.parent_page_id(), 42);

        leaf.set_next_leaf_id(7);
        leaf.set_prev_leaf_id(3);
        assert_eq!(leaf.next_leaf_id(), 7);
        assert_eq!(leaf.prev_leaf_id(), 3);
    }

    #[test]
    fn test_leaf_node_insert_and_read_entries() {
        let mut page = [0u8; PAGE_SIZE];
        let mut leaf = LeafNodeMut::<64>::new(&mut page);
        leaf.init();

        let val1 = [0xAAu8; 64];
        let val2 = [0xBBu8; 64];
        let val3 = [0xCCu8; 64];

        leaf.insert_at(0, 30, &val1);
        leaf.insert_at(0, 10, &val2);
        leaf.insert_at(1, 20, &val3);

        assert_eq!(leaf.num_keys(), 3);
        assert_eq!(leaf.key_at(0), 10);
        assert_eq!(leaf.key_at(1), 20);
        assert_eq!(leaf.key_at(2), 30);
        assert_eq!(leaf.value_at(0)[0], 0xBB);
        assert_eq!(leaf.value_at(1)[0], 0xCC);
        assert_eq!(leaf.value_at(2)[0], 0xAA);
    }

    #[test]
    fn test_leaf_node_binary_search() {
        let mut page = [0u8; PAGE_SIZE];
        let mut leaf = LeafNodeMut::<64>::new(&mut page);
        leaf.init();

        let val = [0u8; 64];
        for i in 0..5u64 {
            let key = (i + 1) * 10;
            leaf.insert_at(i as usize, key, &val);
        }

        assert_eq!(leaf.find_key(10), Ok(0));
        assert_eq!(leaf.find_key(30), Ok(2));
        assert_eq!(leaf.find_key(50), Ok(4));
        assert_eq!(leaf.find_key(5), Err(0));
        assert_eq!(leaf.find_key(25), Err(2));
        assert_eq!(leaf.find_key(55), Err(5));
    }

    #[test]
    fn test_internal_node_read_write() {
        let mut page = [0u8; PAGE_SIZE];
        {
            let mut node = InternalNodeMut::new(&mut page);
            node.init();
        }

        assert_eq!(page[0], NodeType::Internal as u8);

        let mut node = InternalNodeMut::new(&mut page);
        assert_eq!(node.num_keys(), 0);
        assert_eq!(node.parent_page_id(), INVALID_PAGE_ID);

        node.insert_at(0, 100, 5);
        node.set_rightmost_child(10);

        assert_eq!(node.num_keys(), 1);
        assert_eq!(node.key_at(0), 100);
        assert_eq!(node.child_at(0), 5);
        assert_eq!(node.rightmost_child(), 10);
    }

    #[test]
    fn test_internal_node_find_child() {
        let mut page = [0u8; PAGE_SIZE];
        let mut node = InternalNodeMut::new(&mut page);
        node.init();

        node.insert_at(0, 20, 1);
        node.insert_at(1, 40, 2);
        node.insert_at(2, 60, 3);
        node.set_rightmost_child(4);

        assert_eq!(node.find_child(10), 1);
        assert_eq!(node.find_child(20), 2);
        assert_eq!(node.find_child(30), 2);
        assert_eq!(node.find_child(50), 3);
        assert_eq!(node.find_child(60), 4);
        assert_eq!(node.find_child(99), 4);
    }
}
