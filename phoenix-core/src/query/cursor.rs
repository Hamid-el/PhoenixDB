use crate::paging::buffer_pool::BufferPoolManager;
use crate::paging::constants::INVALID_PAGE_ID;
use crate::paging::error::{DbError, Result};
use crate::paging::replacement::ReplacementStrategy;
use crate::paging::types::PageId;
use crate::storage::node::{InternalNode, LeafNode, NodeType, read_node_type};

pub struct BTreeCursor<'a, R: ReplacementStrategy, const VALUE_SIZE: usize> {
    bpm: &'a BufferPoolManager<R>,
    root_page_id: PageId,
    start_key: u64,
    end_key: Option<u64>,
    current_page_id: PageId,
    current_slot: usize,
    exhausted: bool,
    initialized: bool,
}

impl<'a, R: ReplacementStrategy, const VALUE_SIZE: usize> BTreeCursor<'a, R, VALUE_SIZE> {
    pub fn new(
        bpm: &'a BufferPoolManager<R>,
        root_page_id: PageId,
        start_key: u64,
        end_key: Option<u64>,
    ) -> Self {
        Self {
            bpm,
            root_page_id,
            start_key,
            end_key,
            current_page_id: INVALID_PAGE_ID,
            current_slot: 0,
            exhausted: false,
            initialized: false,
        }
    }

    pub fn open(&mut self) -> Result<()> {
        let leaf_page_id = self.find_leaf(self.start_key)?;
        self.current_page_id = leaf_page_id;

        let handle = self.bpm.fetch_page(leaf_page_id)?;
        let data = handle.read();
        let leaf = LeafNode::<VALUE_SIZE>::new(&data);

        let slot = match leaf.find_key(self.start_key) {
            Ok(i) => i,
            Err(i) => i,
        };

        let n = leaf.num_keys() as usize;
        drop(data);
        self.bpm.unpin_page(leaf_page_id, false)?;

        if slot >= n {
            let next = {
                let handle = self.bpm.fetch_page(leaf_page_id)?;
                let data = handle.read();
                let leaf = LeafNode::<VALUE_SIZE>::new(&data);
                let next_id = leaf.next_leaf_id();
                drop(data);
                self.bpm.unpin_page(leaf_page_id, false)?;
                next_id
            };
            if next == INVALID_PAGE_ID {
                self.exhausted = true;
            } else {
                self.current_page_id = next;
                self.current_slot = 0;
            }
        } else {
            self.current_slot = slot;
        }

        self.initialized = true;
        Ok(())
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Vec<u8>>> {
        if self.exhausted {
            return Ok(None);
        }

        loop {
            let handle = self.bpm.fetch_page(self.current_page_id)?;
            let data = handle.read();
            let leaf = LeafNode::<VALUE_SIZE>::new(&data);
            let n = leaf.num_keys() as usize;

            if self.current_slot < n {
                let key = leaf.key_at(self.current_slot);

                if let Some(end) = self.end_key {
                    if key > end {
                        drop(data);
                        self.bpm.unpin_page(self.current_page_id, false)?;
                        self.exhausted = true;
                        return Ok(None);
                    }
                }

                let value = leaf.value_at(self.current_slot);
                let mut record = Vec::with_capacity(8 + VALUE_SIZE);
                record.extend_from_slice(&key.to_le_bytes());
                record.extend_from_slice(value);

                self.current_slot += 1;
                drop(data);
                self.bpm.unpin_page(self.current_page_id, false)?;
                return Ok(Some(record));
            }

            let next = leaf.next_leaf_id();
            drop(data);
            self.bpm.unpin_page(self.current_page_id, false)?;

            if next == INVALID_PAGE_ID {
                self.exhausted = true;
                return Ok(None);
            }

            self.current_page_id = next;
            self.current_slot = 0;
        }
    }

    pub fn close(&mut self) -> Result<()> {
        self.exhausted = true;
        Ok(())
    }

    fn find_leaf(&self, key: u64) -> Result<PageId> {
        let mut page_id = self.root_page_id;

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
}
