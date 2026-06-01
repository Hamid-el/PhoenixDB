use std::collections::HashMap;

use crate::paging::error::Result;
use crate::query::{DynOperator, Operator, Record, Value};

pub struct EquiHashJoin<'a> {
    left_child: DynOperator<'a>,
    right_child: DynOperator<'a>,
    left_join_key: usize,
    right_join_key: usize,
    hash_table: HashMap<Value, Vec<Record>>,
    current_matches: Vec<Record>,
    match_cursor: usize,
    is_open: bool,
}

impl<'a> EquiHashJoin<'a> {
    pub fn new(
        left_child: DynOperator<'a>,
        right_child: DynOperator<'a>,
        left_join_key: usize,
        right_join_key: usize,
    ) -> Self {
        Self {
            left_child,
            right_child,
            left_join_key,
            right_join_key,
            hash_table: HashMap::new(),
            current_matches: Vec::new(),
            match_cursor: 0,
            is_open: false,
        }
    }
}

impl Operator for EquiHashJoin<'_> {
    fn open(&mut self) -> Result<()> {
        self.left_child.open()?;

        while let Some(record) = self.left_child.next()? {
            let key = record.fields[self.left_join_key].clone();
            self.hash_table.entry(key).or_default().push(record);
        }

        self.right_child.open()?;
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

            let right_record = match self.right_child.next()? {
                Some(r) => r,
                None => return Ok(None),
            };

            let right_key = &right_record.fields[self.right_join_key];

            self.current_matches.clear();
            self.match_cursor = 0;

            if let Some(left_records) = self.hash_table.get(right_key) {
                for left_record in left_records {
                    let mut joined_fields = left_record.fields.clone();
                    joined_fields.extend(right_record.fields.iter().cloned());
                    self.current_matches.push(Record::new(joined_fields));
                }
            }
        }
    }

    fn close(&mut self) -> Result<()> {
        self.hash_table.clear();
        self.current_matches.clear();
        self.match_cursor = 0;
        self.is_open = false;
        self.left_child.close()?;
        self.right_child.close()
    }
}
