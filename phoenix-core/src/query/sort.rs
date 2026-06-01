use crate::paging::error::Result;
use crate::query::{DynOperator, Operator, Record};

pub struct Sort<'a> {
    child: DynOperator<'a>,
    cmp: fn(&Record, &Record) -> std::cmp::Ordering,
    sorted_data: Vec<Record>,
    cursor: usize,
    is_open: bool,
}

impl<'a> Sort<'a> {
    pub fn new(child: DynOperator<'a>, cmp: fn(&Record, &Record) -> std::cmp::Ordering) -> Self {
        Self {
            child,
            cmp,
            sorted_data: Vec::new(),
            cursor: 0,
            is_open: false,
        }
    }
}

impl Operator for Sort<'_> {
    fn open(&mut self) -> Result<()> {
        self.child.open()?;

        self.sorted_data.clear();
        while let Some(record) = self.child.next()? {
            self.sorted_data.push(record);
        }

        let cmp = self.cmp;
        self.sorted_data.sort_by(cmp);
        self.cursor = 0;
        self.is_open = true;
        Ok(())
    }

    fn next(&mut self) -> Result<Option<Record>> {
        if self.cursor >= self.sorted_data.len() {
            return Ok(None);
        }
        let record = self.sorted_data[self.cursor].clone();
        self.cursor += 1;
        Ok(Some(record))
    }

    fn close(&mut self) -> Result<()> {
        self.sorted_data.clear();
        self.cursor = 0;
        self.is_open = false;
        self.child.close()
    }
}
