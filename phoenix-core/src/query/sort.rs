use std::fs::File;
use std::io::{Read as IoRead, Seek, SeekFrom, Write as IoWrite};

use tempfile::NamedTempFile;

use crate::paging::error::Result;
use crate::query::{Operator, Record};

pub struct SortConfig {
    pub max_memory_bytes: usize,
    pub max_k: usize,
}

impl Default for SortConfig {
    fn default() -> Self {
        Self {
            max_memory_bytes: 64 * 1024 * 1024,
            max_k: 16,
        }
    }
}

pub struct Sort<'a> {
    source: Box<dyn Operator + 'a>,
    key_fn: fn(&Record) -> u64,
    config: SortConfig,
    state: SortState,
}

enum SortState {
    Uninitialized,
    InMemory {
        records: Vec<Record>,
        cursor: usize,
    },
    OnDisk {
        iter: VarLengthRunIterator,
    },
}

impl<'a> Sort<'a> {
    pub fn new(
        source: Box<dyn Operator + 'a>,
        key_fn: fn(&Record) -> u64,
        config: SortConfig,
    ) -> Self {
        Self {
            source,
            key_fn,
            config,
            state: SortState::Uninitialized,
        }
    }
}

impl Operator for Sort<'_> {
    fn open(&mut self) -> Result<()> {
        self.source.open()?;

        let mut records: Vec<Record> = Vec::new();
        let mut total_bytes: usize = 0;

        while let Some(record) = self.source.next()? {
            total_bytes += record.len();
            records.push(record);
        }

        let key_fn = self.key_fn;

        if total_bytes <= self.config.max_memory_bytes {
            records.sort_by_key(&key_fn);
            self.state = SortState::InMemory { records, cursor: 0 };
        } else {
            let iter = disk_sort(records, key_fn, &self.config)?;
            self.state = SortState::OnDisk { iter };
        }

        Ok(())
    }

    fn next(&mut self) -> Result<Option<Record>> {
        match &mut self.state {
            SortState::Uninitialized => Ok(None),
            SortState::InMemory {
                records,
                cursor
            } => {
                if *cursor >= records.len() {
                    return Ok(None);
                }
                let record = records[*cursor].clone();
                *cursor += 1;
                Ok(Some(record))
            }
            SortState::OnDisk { iter } => iter.next_record(),
        }
    }

    fn close(&mut self) -> Result<()> {
        self.state = SortState::Uninitialized;
        self.source.close()
    }
}

struct RunDescriptor {
    start_offset: u64,
    num_records: u64,
}

struct VarRunFile {
    file: File,
    _temp: NamedTempFile,
    write_pos: u64,
}

impl VarRunFile {
    fn new() -> Result<Self> {
        let temp = NamedTempFile::new()?;
        let file = temp.reopen()?;
        Ok(Self {
            file,
            _temp: temp,
            write_pos: 0,
        })
    }

    fn write_record(&mut self, record: &[u8]) -> Result<()> {
        let len = record.len() as u16;
        self.file.seek(SeekFrom::Start(self.write_pos))?;
        self.file.write_all(&len.to_le_bytes())?;
        self.file.write_all(record)?;
        self.write_pos += 2 + record.len() as u64;
        Ok(())
    }

    fn read_record_at(&mut self, offset: u64) -> Result<Option<(Record, u64)>> {
        self.file.seek(SeekFrom::Start(offset))?;
        let mut len_buf = [0u8; 2];
        if self.file.read_exact(&mut len_buf).is_err() {
            return Ok(None);
        }
        let len = u16::from_le_bytes(len_buf) as usize;
        let mut record = vec![0u8; len];
        self.file.read_exact(&mut record)?;
        Ok(Some((record, offset + 2 + len as u64)))
    }
}

fn disk_sort(
    mut records: Vec<Record>,
    key_fn: fn(&Record) -> u64,
    config: &SortConfig,
) -> Result<VarLengthRunIterator> {
    let mut run_file = VarRunFile::new()?;
    let max_k = config.max_k.max(2);

    let chunk_size = config.max_memory_bytes / std::mem::size_of::<Record>().max(64);
    let chunk_size = chunk_size.max(1);

    let mut runs: Vec<RunDescriptor> = Vec::new();

    for chunk in records.chunks_mut(chunk_size) {
        chunk.sort_by_key(&key_fn);
        let start_offset = run_file.write_pos;
        let num_records = chunk.len() as u64;
        for record in chunk.iter() {
            run_file.write_record(record)?;
        }
        runs.push(RunDescriptor {
            start_offset,
            num_records,
        });
    }

    let final_run = merge_runs(&mut run_file, runs, key_fn, max_k)?;

    VarLengthRunIterator::new(run_file, final_run)
}

struct RunReader {
    current_offset: u64,
    records_remaining: u64,
    current_record: Option<Record>,
}

impl RunReader {
    fn new(run_file: &mut VarRunFile, desc: &RunDescriptor) -> Result<Self> {
        let mut reader = Self {
            current_offset: desc.start_offset,
            records_remaining: desc.num_records,
            current_record: None,
        };
        reader.advance(run_file)?;
        Ok(reader)
    }

    fn advance(&mut self, run_file: &mut VarRunFile) -> Result<()> {
        if self.records_remaining == 0 {
            self.current_record = None;
            return Ok(());
        }
        if let Some((record, new_offset)) = run_file.read_record_at(self.current_offset)? {
            self.current_offset = new_offset;
            self.records_remaining -= 1;
            self.current_record = Some(record);
        } else {
            self.current_record = None;
        }
        Ok(())
    }

    fn is_exhausted(&self) -> bool {
        self.current_record.is_none()
    }

    fn peek_key(&self, key_fn: fn(&Record) -> u64) -> Option<u64> {
        self.current_record.as_ref().map(key_fn)
    }

    fn take_record(&mut self) -> Option<Record> {
        self.current_record.take()
    }
}

struct HeapEntry {
    key: u64,
    run_index: usize,
}

struct MinHeap {
    entries: Vec<HeapEntry>,
}

impl MinHeap {
    fn with_capacity(cap: usize) -> Self {
        Self {
            entries: Vec::with_capacity(cap),
        }
    }

    fn push(&mut self, entry: HeapEntry) {
        self.entries.push(entry);
        self.sift_up(self.entries.len() - 1);
    }

    fn pop(&mut self) -> Option<HeapEntry> {
        if self.entries.is_empty() {
            return None;
        }
        let last = self.entries.len() - 1;
        self.entries.swap(0, last);
        let result = self.entries.pop();
        if !self.entries.is_empty() {
            self.sift_down(0);
        }
        result
    }

    fn sift_up(&mut self, mut idx: usize) {
        while idx > 0 {
            let parent = (idx - 1) / 2;
            if self.is_less(idx, parent) {
                self.entries.swap(idx, parent);
                idx = parent;
            } else {
                break;
            }
        }
    }

    fn sift_down(&mut self, mut idx: usize) {
        let len = self.entries.len();
        loop {
            let left = 2 * idx + 1;
            let right = 2 * idx + 2;
            let mut smallest = idx;
            if left < len && self.is_less(left, smallest) {
                smallest = left;
            }
            if right < len && self.is_less(right, smallest) {
                smallest = right;
            }
            if smallest != idx {
                self.entries.swap(idx, smallest);
                idx = smallest;
            } else {
                break;
            }
        }
    }

    fn is_less(&self, a: usize, b: usize) -> bool {
        self.entries[a].key < self.entries[b].key
            || (self.entries[a].key == self.entries[b].key
                && self.entries[a].run_index < self.entries[b].run_index)
    }
}

fn merge_runs(
    run_file: &mut VarRunFile,
    mut runs: Vec<RunDescriptor>,
    key_fn: fn(&Record) -> u64,
    max_k: usize,
) -> Result<Option<RunDescriptor>> {
    if runs.is_empty() {
        return Ok(None);
    }

    while runs.len() > 1 {
        let mut new_runs = Vec::new();
        let mut i = 0;

        while i < runs.len() {
            let chunk_end = (i + max_k).min(runs.len());

            if chunk_end - i == 1 {
                new_runs.push(RunDescriptor {
                    start_offset: runs[i].start_offset,
                    num_records: runs[i].num_records,
                });
                i = chunk_end;
                continue;
            }

            let mut readers: Vec<RunReader> = Vec::new();
            for desc in &runs[i..chunk_end] {
                readers.push(RunReader::new(run_file, desc)?);
            }

            let mut heap = MinHeap::with_capacity(readers.len());
            for (idx, reader) in readers.iter().enumerate() {
                if let Some(key) = reader.peek_key(key_fn) {
                    heap.push(HeapEntry {
                        key,
                        run_index: idx,
                    });
                }
            }

            let start_offset = run_file.write_pos;
            let mut num_records: u64 = 0;

            while let Some(entry) = heap.pop() {
                let reader = &mut readers[entry.run_index];
                if let Some(record) = reader.take_record() {
                    run_file.write_record(&record)?;
                    num_records += 1;
                }

                reader.advance(run_file)?;
                if !reader.is_exhausted() {
                    if let Some(key) = reader.peek_key(key_fn) {
                        heap.push(HeapEntry {
                            key,
                            run_index: entry.run_index,
                        });
                    }
                }
            }

            new_runs.push(RunDescriptor {
                start_offset,
                num_records,
            });
            i = chunk_end;
        }

        runs = new_runs;
    }

    Ok(runs.into_iter().next())
}

pub struct VarLengthRunIterator {
    run_file: VarRunFile,
    current_offset: u64,
    records_remaining: u64,
}

impl VarLengthRunIterator {
    fn new(run_file: VarRunFile, run: Option<RunDescriptor>) -> Result<Self> {
        let (offset, count) = match run {
            Some(r) => (r.start_offset, r.num_records),
            None => (0, 0),
        };
        Ok(Self {
            run_file,
            current_offset: offset,
            records_remaining: count,
        })
    }

    fn next_record(&mut self) -> Result<Option<Record>> {
        if self.records_remaining == 0 {
            return Ok(None);
        }
        match self.run_file.read_record_at(self.current_offset)? {
            Some((record, new_offset)) => {
                self.current_offset = new_offset;
                self.records_remaining -= 1;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }
}
