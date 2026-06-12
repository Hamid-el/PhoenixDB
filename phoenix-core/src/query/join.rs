use std::collections::HashMap;

use crate::paging::error::Result;
use crate::query::{Operator, Record};

pub struct HashJoin<'a> {
    left: Box<dyn Operator + 'a>,
    right: Box<dyn Operator + 'a>,
    left_key_fn: fn(&Record) -> u64,
    right_key_fn: fn(&Record) -> u64,
    hash_table: HashMap<u64, Vec<Record>>,
    current_matches: Vec<Record>,
    match_cursor: usize,
    is_open: bool,
}

impl<'a> HashJoin<'a> {
    pub fn new(
        left: Box<dyn Operator + 'a>,
        right: Box<dyn Operator + 'a>,
        left_key_fn: fn(&Record) -> u64,
        right_key_fn: fn(&Record) -> u64,
    ) -> Self {
        Self {
            left,
            right,
            left_key_fn,
            right_key_fn,
            hash_table: HashMap::new(),
            current_matches: Vec::new(),
            match_cursor: 0,
            is_open: false,
        }
    }
}

impl Operator for HashJoin<'_> {
    fn open(&mut self) -> Result<()> {
        self.left.open()?;

        while let Some(record) = self.left.next()? {
            let key = (self.left_key_fn)(&record);
            self.hash_table.entry(key).or_default().push(record);
        }

        self.right.open()?;
        self.is_open = true;
        Ok(())
    }

    fn next(&mut self) -> Result<Option<Record>> {
        loop {
            if self.match_cursor < self.current_matches.len() {
                let record = self.current_matches[self.match_cursor].clone();
                self.match_cursor += 1;
                return Ok(Some(record));
            }

            let right_record = match self.right.next()? {
                Some(r) => r,
                None => return Ok(None),
            };

            let right_key = (self.right_key_fn)(&right_record);

            self.current_matches.clear();
            self.match_cursor = 0;

            if let Some(left_records) = self.hash_table.get(&right_key) {
                for left_record in left_records {
                    let mut joined = Vec::with_capacity(left_record.len() + right_record.len());
                    joined.extend_from_slice(left_record);
                    joined.extend_from_slice(&right_record);
                    self.current_matches.push(joined);
                }
            }
        }
    }

    fn close(&mut self) -> Result<()> {
        self.hash_table.clear();
        self.current_matches.clear();
        self.match_cursor = 0;
        self.is_open = false;
        self.left.close()?;
        self.right.close()
    }
}
