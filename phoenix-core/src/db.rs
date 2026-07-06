use std::sync::{Arc, RwLock};

use crate::paging::buffer_pool::BufferPoolManager;
use crate::paging::disk_manager::DiskManager;
use crate::paging::error::Result;
use crate::paging::replacement::ReplacementStrategy;
use crate::paging::types::PageId;
use crate::storage::btree::BPlusTree;

pub struct Database<R: ReplacementStrategy, const VALUE_SIZE: usize> {
    bpm: BufferPoolManager<R>,
}

impl<R: ReplacementStrategy, const VALUE_SIZE: usize> Database<R, VALUE_SIZE> {
    pub fn new(pool_size: usize, disk_manager: DiskManager, replacer: R) -> Self {
        Self {
            bpm: BufferPoolManager::new(pool_size, disk_manager, replacer),
        }
    }

    pub fn bpm(&self) -> &BufferPoolManager<R> {
        &self.bpm
    }

    pub fn create_table(&self) -> Result<PageId> {
        let tree = BPlusTree::<R, VALUE_SIZE>::create(&self.bpm)?;
        Ok(tree.root_page_id())
    }

    pub fn open_table(&self, root_page_id: PageId) -> BPlusTree<'_, R, VALUE_SIZE> {
        BPlusTree::open(&self.bpm, root_page_id)
    }
}

pub type SharedDatabase<R, const VALUE_SIZE: usize> = Arc<RwLock<Database<R, VALUE_SIZE>>>;

pub fn shared_database<R: ReplacementStrategy, const VALUE_SIZE: usize>(
    pool_size: usize,
    disk_manager: DiskManager,
    replacer: R,
) -> SharedDatabase<R, VALUE_SIZE> {
    Arc::new(RwLock::new(Database::new(
        pool_size,
        disk_manager,
        replacer,
    )))
}
