use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use tempfile::NamedTempFile;

use crate::paging::constants::PAGE_SIZE;
use crate::paging::error::Result;

const RUN_PAGE_HEADER_SIZE: usize = 2;

const fn records_per_page(record_size: usize) -> usize {
    (PAGE_SIZE - RUN_PAGE_HEADER_SIZE) / record_size
}

pub struct SortConfig {
    pub buffer_pages: usize,
    pub max_k: usize,
}

impl Default for SortConfig {
    fn default() -> Self {
        Self {
            buffer_pages: 64,
            max_k: 16,
        }
    }
}

struct RunDescriptor {
    start_page: u32,
    num_pages: u32,
    total_records: u64,
}

struct RunFile {
    file: File,
    _temp: NamedTempFile,
    num_pages: u32,
}

impl RunFile {
    fn new() -> Result<Self> {
        let temp = NamedTempFile::new()?;
        let file = temp.reopen()?;
        Ok(Self {
            file,
            _temp: temp,
            num_pages: 0,
        })
    }

    fn append_page(&mut self, data: &[u8; PAGE_SIZE]) -> Result<u32> {
        let page_id = self.num_pages;
        let offset = page_id as u64 * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(data)?;
        self.num_pages += 1;
        Ok(page_id)
    }

    fn read_page(&mut self, page_id: u32, buf: &mut [u8; PAGE_SIZE]) -> Result<()> {
        let offset = page_id as u64 * PAGE_SIZE as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)?;
        Ok(())
    }
}

fn get_record_count(page: &[u8; PAGE_SIZE]) -> u16 {
    u16::from_le_bytes([page[0], page[1]])
}

fn set_record_count(page: &mut [u8; PAGE_SIZE], count: u16) {
    page[0..2].copy_from_slice(&count.to_le_bytes());
}

fn write_record_to_page<const RECORD_SIZE: usize>(
    page: &mut [u8; PAGE_SIZE],
    index: usize,
    record: &[u8; RECORD_SIZE],
) {
    let offset = RUN_PAGE_HEADER_SIZE + index * RECORD_SIZE;
    page[offset..offset + RECORD_SIZE].copy_from_slice(record);
}

fn read_record_from_page<const RECORD_SIZE: usize>(
    page: &[u8; PAGE_SIZE],
    index: usize,
) -> [u8; RECORD_SIZE] {
    let offset = RUN_PAGE_HEADER_SIZE + index * RECORD_SIZE;
    let mut record = [0u8; RECORD_SIZE];
    record.copy_from_slice(&page[offset..offset + RECORD_SIZE]);
    record
}

struct HeapEntry<const RECORD_SIZE: usize> {
    key: u64,
    run_index: usize,
    record: [u8; RECORD_SIZE],
}

struct MinHeap<const RECORD_SIZE: usize> {
    entries: Vec<HeapEntry<RECORD_SIZE>>,
}

impl<const RECORD_SIZE: usize> MinHeap<RECORD_SIZE> {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
        }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn push(&mut self, entry: HeapEntry<RECORD_SIZE>) {
        self.entries.push(entry);
        self.sift_up(self.entries.len() - 1);
    }

    fn pop(&mut self) -> Option<HeapEntry<RECORD_SIZE>> {
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
            if self.entries[idx].key < self.entries[parent].key
                || (self.entries[idx].key == self.entries[parent].key
                    && self.entries[idx].run_index < self.entries[parent].run_index)
            {
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

struct RunReader<const RECORD_SIZE: usize> {
    run: RunDescriptor,
    current_page: u32,
    current_record_idx: usize,
    records_in_page: usize,
    page_buf: [u8; PAGE_SIZE],
    total_emitted: u64,
}

impl<const RECORD_SIZE: usize> RunReader<RECORD_SIZE> {
    fn new(run_file: &mut RunFile, run: RunDescriptor) -> Result<Self> {
        let mut page_buf = [0u8; PAGE_SIZE];
        run_file.read_page(run.start_page, &mut page_buf)?;
        let records_in_page = get_record_count(&page_buf) as usize;
        Ok(Self {
            run,
            current_page: 0,
            current_record_idx: 0,
            records_in_page,
            page_buf,
            total_emitted: 0,
        })
    }

    fn is_exhausted(&self) -> bool {
        self.total_emitted >= self.run.total_records
    }

    fn peek_key<F>(&self, key_fn: &F) -> Option<u64>
    where
        F: Fn(&[u8; RECORD_SIZE]) -> u64,
    {
        if self.is_exhausted() {
            return None;
        }
        let record = read_record_from_page::<RECORD_SIZE>(&self.page_buf, self.current_record_idx);
        Some(key_fn(&record))
    }

    fn next_record(&mut self, run_file: &mut RunFile) -> Result<Option<[u8; RECORD_SIZE]>> {
        if self.is_exhausted() {
            return Ok(None);
        }

        let record = read_record_from_page::<RECORD_SIZE>(&self.page_buf, self.current_record_idx);
        self.current_record_idx += 1;
        self.total_emitted += 1;

        if self.current_record_idx >= self.records_in_page && !self.is_exhausted() {
            self.current_page += 1;
            let abs_page = self.run.start_page + self.current_page;
            run_file.read_page(abs_page, &mut self.page_buf)?;
            self.records_in_page = get_record_count(&self.page_buf) as usize;
            self.current_record_idx = 0;
        }

        Ok(Some(record))
    }
}

struct OutputBuffer<const RECORD_SIZE: usize> {
    page_buf: [u8; PAGE_SIZE],
    record_count: usize,
    start_page: u32,
    pages_written: u32,
    total_records: u64,
}

impl<const RECORD_SIZE: usize> OutputBuffer<RECORD_SIZE> {
    fn new() -> Self {
        Self {
            page_buf: [0u8; PAGE_SIZE],
            record_count: 0,
            start_page: 0,
            pages_written: 0,
            total_records: 0,
        }
    }

    fn add_record(&mut self, record: &[u8; RECORD_SIZE], run_file: &mut RunFile) -> Result<()> {
        let max = records_per_page(RECORD_SIZE);
        if self.record_count >= max {
            self.flush_page(run_file)?;
        }
        write_record_to_page::<RECORD_SIZE>(&mut self.page_buf, self.record_count, record);
        self.record_count += 1;
        self.total_records += 1;
        Ok(())
    }

    fn flush_page(&mut self, run_file: &mut RunFile) -> Result<()> {
        if self.record_count == 0 {
            return Ok(());
        }
        set_record_count(&mut self.page_buf, self.record_count as u16);
        let page_id = run_file.append_page(&self.page_buf)?;
        if self.pages_written == 0 {
            self.start_page = page_id;
        }
        self.pages_written += 1;
        self.page_buf = [0u8; PAGE_SIZE];
        self.record_count = 0;
        Ok(())
    }

    fn finish(mut self, run_file: &mut RunFile) -> Result<RunDescriptor> {
        self.flush_page(run_file)?;
        Ok(RunDescriptor {
            start_page: self.start_page,
            num_pages: self.pages_written,
            total_records: self.total_records,
        })
    }
}

fn generate_runs<const RECORD_SIZE: usize, I, F>(
    records: &mut I,
    key_fn: &F,
    buffer_pages: usize,
    run_file: &mut RunFile,
) -> Result<Vec<RunDescriptor>>
where
    I: Iterator<Item = [u8; RECORD_SIZE]>,
    F: Fn(&[u8; RECORD_SIZE]) -> u64,
{
    let rpp = records_per_page(RECORD_SIZE);
    let buffer_capacity = buffer_pages * rpp;
    let mut buffer: Vec<[u8; RECORD_SIZE]> = Vec::with_capacity(buffer_capacity);
    let mut runs = Vec::new();

    loop {
        buffer.clear();
        for record in records.by_ref() {
            buffer.push(record);
            if buffer.len() >= buffer_capacity {
                break;
            }
        }

        if buffer.is_empty() {
            break;
        }

        buffer.sort_unstable_by_key(|rec| key_fn(rec));

        let mut output = OutputBuffer::<RECORD_SIZE>::new();
        for record in &buffer {
            output.add_record(record, run_file)?;
        }
        let run = output.finish(run_file)?;
        runs.push(run);
    }

    Ok(runs)
}

fn merge_runs<const RECORD_SIZE: usize, F>(
    mut runs: Vec<RunDescriptor>,
    key_fn: &F,
    max_k: usize,
    run_file: &mut RunFile,
) -> Result<Option<RunDescriptor>>
where
    F: Fn(&[u8; RECORD_SIZE]) -> u64,
{
    if runs.is_empty() {
        return Ok(None);
    }

    while runs.len() > 1 {
        let mut new_runs = Vec::new();

        let mut i = 0;
        while i < runs.len() {
            let chunk_end = (i + max_k).min(runs.len());
            let chunk_len = chunk_end - i;

            if chunk_len == 1 {
                new_runs.push(RunDescriptor {
                    start_page: runs[i].start_page,
                    num_pages: runs[i].num_pages,
                    total_records: runs[i].total_records,
                });
                i = chunk_end;
                continue;
            }

            let mut readers: Vec<RunReader<RECORD_SIZE>> = Vec::with_capacity(chunk_len);

            for run_desc in &runs[i..chunk_end] {
                let rd = RunDescriptor {
                    start_page: run_desc.start_page,
                    num_pages: run_desc.num_pages,
                    total_records: run_desc.total_records,
                };
                readers.push(RunReader::new(run_file, rd)?);
            }

            let mut heap = MinHeap::<RECORD_SIZE>::with_capacity(readers.len());
            for (i, reader) in readers.iter().enumerate() {
                if let Some(key) = reader.peek_key(key_fn) {
                    let record =
                        read_record_from_page::<RECORD_SIZE>(&reader.page_buf, reader.current_record_idx);
                    heap.push(HeapEntry {
                        key,
                        run_index: i,
                        record,
                    });
                }
            }

            let mut output = OutputBuffer::<RECORD_SIZE>::new();

            while let Some(entry) = heap.pop() {
                output.add_record(&entry.record, run_file)?;

                let reader = &mut readers[entry.run_index];
                reader.next_record(run_file)?;

                if !reader.is_exhausted() {
                    let key = reader.peek_key(key_fn).unwrap();
                    let record = read_record_from_page::<RECORD_SIZE>(
                        &reader.page_buf,
                        reader.current_record_idx,
                    );
                    heap.push(HeapEntry {
                        key,
                        run_index: entry.run_index,
                        record,
                    });
                }
            }

            let merged_run = output.finish(run_file)?;
            new_runs.push(merged_run);

            i = chunk_end;
        }

        runs = new_runs;
    }

    Ok(runs.into_iter().next())
}

pub struct SortedRunIterator<const RECORD_SIZE: usize> {
    run_file: RunFile,
    reader: Option<RunReader<RECORD_SIZE>>,
}

impl<const RECORD_SIZE: usize> SortedRunIterator<RECORD_SIZE> {
    fn new(mut run_file: RunFile, run: Option<RunDescriptor>) -> Result<Self> {
        let reader = match run {
            Some(r) if r.total_records > 0 => Some(RunReader::new(&mut run_file, r)?),
            _ => None,
        };
        Ok(Self { run_file, reader })
    }
}

impl<const RECORD_SIZE: usize> Iterator for SortedRunIterator<RECORD_SIZE> {
    type Item = Result<[u8; RECORD_SIZE]>;

    fn next(&mut self) -> Option<Self::Item> {
        let reader = self.reader.as_mut()?;
        if reader.is_exhausted() {
            return None;
        }
        match reader.next_record(&mut self.run_file) {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

pub struct ExternalSort<const RECORD_SIZE: usize>;

impl<const RECORD_SIZE: usize> ExternalSort<RECORD_SIZE> {
    pub fn sort<I, F>(
        records: I,
        key_fn: F,
        config: SortConfig,
    ) -> Result<SortedRunIterator<RECORD_SIZE>>
    where
        I: Iterator<Item = [u8; RECORD_SIZE]>,
        F: Fn(&[u8; RECORD_SIZE]) -> u64,
    {
        assert!(
            RECORD_SIZE > 0 && RECORD_SIZE <= PAGE_SIZE - RUN_PAGE_HEADER_SIZE,
            "RECORD_SIZE must be between 1 and {} bytes",
            PAGE_SIZE - RUN_PAGE_HEADER_SIZE
        );

        let buffer_pages = config.buffer_pages.max(1);
        let max_k = config.max_k.max(2);

        let mut run_file = RunFile::new()?;
        let mut records = records;

        let runs = generate_runs::<RECORD_SIZE, _, _>(
            &mut records,
            &key_fn,
            buffer_pages,
            &mut run_file,
        )?;

        let final_run = merge_runs::<RECORD_SIZE, _>(runs, &key_fn, max_k, &mut run_file)?;

        SortedRunIterator::new(run_file, final_run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_heap_basic() {
        let mut heap = MinHeap::<8>::with_capacity(4);
        heap.push(HeapEntry { key: 30, run_index: 0, record: [0; 8] });
        heap.push(HeapEntry { key: 10, run_index: 1, record: [1; 8] });
        heap.push(HeapEntry { key: 20, run_index: 2, record: [2; 8] });
        heap.push(HeapEntry { key: 5, run_index: 3, record: [3; 8] });

        assert_eq!(heap.pop().unwrap().key, 5);
        assert_eq!(heap.pop().unwrap().key, 10);
        assert_eq!(heap.pop().unwrap().key, 20);
        assert_eq!(heap.pop().unwrap().key, 30);
        assert!(heap.pop().is_none());
    }

    #[test]
    fn test_min_heap_duplicates() {
        let mut heap = MinHeap::<8>::with_capacity(4);
        heap.push(HeapEntry { key: 5, run_index: 2, record: [2; 8] });
        heap.push(HeapEntry { key: 5, run_index: 0, record: [0; 8] });
        heap.push(HeapEntry { key: 5, run_index: 1, record: [1; 8] });

        let a = heap.pop().unwrap();
        let b = heap.pop().unwrap();
        let c = heap.pop().unwrap();
        assert_eq!(a.run_index, 0);
        assert_eq!(b.run_index, 1);
        assert_eq!(c.run_index, 2);
        assert!(heap.is_empty());
    }

    #[test]
    fn test_page_serialization_roundtrip() {
        let mut page = [0u8; PAGE_SIZE];
        let records: [[u8; 16]; 3] = [[0xAA; 16], [0xBB; 16], [0xCC; 16]];

        for (i, rec) in records.iter().enumerate() {
            write_record_to_page::<16>(&mut page, i, rec);
        }
        set_record_count(&mut page, 3);

        assert_eq!(get_record_count(&page), 3);
        for (i, expected) in records.iter().enumerate() {
            let got = read_record_from_page::<16>(&page, i);
            assert_eq!(&got, expected);
        }
    }

    #[test]
    fn test_records_per_page_calculation() {
        assert_eq!(records_per_page(8), (PAGE_SIZE - 2) / 8);
        assert_eq!(records_per_page(64), (PAGE_SIZE - 2) / 64);
        assert_eq!(records_per_page(72), (PAGE_SIZE - 2) / 72);
        assert_eq!(records_per_page(256), (PAGE_SIZE - 2) / 256);
    }
}
