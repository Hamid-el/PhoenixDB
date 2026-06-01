pub mod cursor;
pub mod join;
pub mod scan;
pub mod sort;
pub mod types;

pub use types::{Record, Value};

use crate::paging::error::Result;

pub trait Operator {
    fn open(&mut self) -> Result<()>;
    fn next(&mut self) -> Result<Option<Record>>;
    fn close(&mut self) -> Result<()>;
}

pub type DynOperator<'a> = Box<dyn Operator + 'a>;
