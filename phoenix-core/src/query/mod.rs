pub mod cursor;
pub mod join;
pub mod scan;
pub mod sort;
mod types;
mod bitmap_scan;

use crate::paging::error::Result;

pub type Record = Vec<u8>;

pub trait Operator {
    fn open(&mut self) -> Result<()>;
    fn next(&mut self) -> Result<Option<Record>>;
    fn close(&mut self) -> Result<()>;
}
