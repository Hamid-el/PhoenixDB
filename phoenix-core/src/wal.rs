
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{debug, info, warn};

use crate::encoding::{ByteReader, ByteWriter};
use crate::paging::constants::PAGE_SIZE;
use crate::paging::error::{DbError, Result};
use crate::paging::types::PageId;

const REC_PAGE_IMAGE: u8 = 1;
const REC_COMMIT: u8 = 2;

///  WAL reconstructs the latest committed after-image
/// of each page and the catalog as of the last commit
pub struct RecoveredState {
    pub pages: HashMap<PageId, Box<[u8; PAGE_SIZE]>>,
    pub catalog_bytes: Vec<u8>,
}

pub struct Wal {
    path: PathBuf,
    file: Mutex<File>,
}

impl Wal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        file.seek(SeekFrom::End(0))?;

        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }


    pub fn append_commit(
        &self,
        pages: &[(PageId, Box<[u8; PAGE_SIZE]>)],
        catalog_bytes: &[u8],
    ) -> Result<()> {
        let mut buf = Vec::new();

        for (page_id, data) in pages {
            let mut body = ByteWriter::new();
            body.put_u8(REC_PAGE_IMAGE);
            body.put_u32(*page_id);
            body.put_bytes(data.as_ref());
            write_record(&mut buf, &body.into_bytes());
        }

        let mut body = ByteWriter::new();
        body.put_u8(REC_COMMIT);
        body.put_u32(catalog_bytes.len() as u32);
        body.put_bytes(catalog_bytes);
        write_record(&mut buf, &body.into_bytes());

        let mut file = self
            .file
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;
        file.write_all(&buf)?;
        file.sync_data()?;

        debug!(
            "WAL commit: {} page image(s), {} bytes total",
            pages.len(),
            buf.len()
        );
        Ok(())
    }

    pub fn size(&self) -> Result<u64> {
        let file = self
            .file
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;
        Ok(file.metadata()?.len())
    }

    /// Empties the WAL. Only call this after the data file and catalog have
    /// been durably written (checkpoint) — otherwise committed data is lost
    pub fn truncate(&self) -> Result<()> {
        let mut file = self
            .file
            .lock()
            .map_err(|e| DbError::Internal(e.to_string()))?;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.sync_all()?;
        info!("WAL truncated (checkpoint)");
        Ok(())
    }


    pub fn read_committed(path: impl AsRef<Path>) -> Result<Option<RecoveredState>> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(None);
        }

        let mut bytes = Vec::new();
        File::open(path)?.read_to_end(&mut bytes)?;

        let mut committed_pages: HashMap<PageId, Box<[u8; PAGE_SIZE]>> = HashMap::new();
        let mut committed_catalog: Option<Vec<u8>> = None;
        let mut pending_pages: Vec<(PageId, Box<[u8; PAGE_SIZE]>)> = Vec::new();

        let mut pos = 0usize;
        loop {
            let Some(body) = read_record(&bytes, &mut pos) else {
                break; // clean end or torn tail
            };

            let mut r = ByteReader::new(body);
            match r.get_u8()? {
                REC_PAGE_IMAGE => {
                    let page_id = r.get_u32()?;
                    let data = r.get_bytes(PAGE_SIZE)?;
                    let mut page = Box::new([0u8; PAGE_SIZE]);
                    page.copy_from_slice(data);
                    pending_pages.push((page_id, page));
                }
                REC_COMMIT => {
                    let len = r.get_u32()? as usize;
                    let catalog = r.get_bytes(len)?.to_vec();
                    // commit record seals all images since the previous
                    // commit; later images of the same page win
                    for (page_id, page) in pending_pages.drain(..) {
                        committed_pages.insert(page_id, page);
                    }
                    committed_catalog = Some(catalog);
                }
                other => {
                    warn!("WAL: unknown record kind {} — stopping replay here", other);
                    break;
                }
            }
        }

        if !pending_pages.is_empty() {
            info!(
                "WAL: discarding {} uncommitted page image(s) from torn tail",
                pending_pages.len()
            );
        }

        match committed_catalog {
            Some(catalog_bytes) => {
                info!(
                    "WAL replay: {} committed page image(s) recovered",
                    committed_pages.len()
                );
                Ok(Some(RecoveredState {
                    pages: committed_pages,
                    catalog_bytes,
                }))
            }
            None => Ok(None),
        }
    }
}

/// On-disk record framing: [len: u32][crc32(body): u32][body: len bytes].
fn write_record(out: &mut Vec<u8>, body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(body).to_le_bytes());
    out.extend_from_slice(body);
}

/// Reads one framed record, advancing `pos`. Returns `None` on clean EOF or
/// on any inconsistency (truncated frame, CRC mismatch)
fn read_record<'a>(bytes: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    let remaining = bytes.len() - *pos;
    if remaining < 8 {
        return None;
    }
    let len = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().unwrap()) as usize;
    let crc = u32::from_le_bytes(bytes[*pos + 4..*pos + 8].try_into().unwrap());
    if len == 0 || remaining < 8 + len {
        return None;
    }
    let body = &bytes[*pos + 8..*pos + 8 + len];
    if crc32(body) != crc {
        warn!("WAL: CRC mismatch at offset {} — torn record", *pos);
        return None;
    }
    *pos += 8 + len;
    Some(body)
}

/// Standard CRC-32 (IEEE 802.3), bitwise implementation.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn page_filled(byte: u8) -> Box<[u8; PAGE_SIZE]> {
        Box::new([byte; PAGE_SIZE])
    }

    #[test]
    fn test_crc32_known_value() {
        // CRC-32 of "123456789" is the classic check value 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn test_missing_wal_is_none() {
        let dir = tempdir().unwrap();
        let result = Wal::read_committed(dir.path().join("nope.wal")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_commit_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        let wal = Wal::open(&path).unwrap();
        wal.append_commit(&[(0, page_filled(0xAA)), (3, page_filled(0xBB))], b"cat-v1")
            .unwrap();
        wal.append_commit(&[(3, page_filled(0xCC))], b"cat-v2").unwrap();
        drop(wal);

        let state = Wal::read_committed(&path).unwrap().unwrap();
        assert_eq!(state.catalog_bytes, b"cat-v2");
        assert_eq!(state.pages.len(), 2);
        assert_eq!(state.pages[&0][0], 0xAA);
        // Later image of page 3 wins
        assert_eq!(state.pages[&3][0], 0xCC);
    }

    #[test]
    fn test_torn_tail_ignored() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        let wal = Wal::open(&path).unwrap();
        wal.append_commit(&[(1, page_filled(0x11))], b"cat-v1").unwrap();
        wal.append_commit(&[(2, page_filled(0x22))], b"cat-v2").unwrap();
        drop(wal);

        // Simulate a crash mid-append with chop bytes off the end
        let len = std::fs::metadata(&path).unwrap().len();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(len - 100).unwrap();

        let state = Wal::read_committed(&path).unwrap().unwrap();
        // First commit survives; the torn second one is rolled back.
        assert_eq!(state.catalog_bytes, b"cat-v1");
        assert_eq!(state.pages.len(), 1);
        assert_eq!(state.pages[&1][0], 0x11);
    }

    #[test]
    fn test_uncommitted_images_ignored() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        // Write a page image with no commit record after it
        let mut buf = Vec::new();
        let mut body = ByteWriter::new();
        body.put_u8(REC_PAGE_IMAGE);
        body.put_u32(7);
        body.put_bytes(page_filled(0x77).as_ref());
        write_record(&mut buf, &body.into_bytes());
        std::fs::write(&path, &buf).unwrap();

        let result = Wal::read_committed(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_truncate_resets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        let wal = Wal::open(&path).unwrap();
        wal.append_commit(&[(0, page_filled(0xAA))], b"cat").unwrap();
        assert!(wal.size().unwrap() > 0);

        wal.truncate().unwrap();
        assert_eq!(wal.size().unwrap(), 0);
        drop(wal);

        assert!(Wal::read_committed(&path).unwrap().is_none());
    }

    #[test]
    fn test_corrupted_record_stops_replay() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.wal");

        let wal = Wal::open(&path).unwrap();
        wal.append_commit(&[(1, page_filled(0x11))], b"cat-v1").unwrap();
        drop(wal);

        // Flip a byte inside the record body.
        let mut bytes = std::fs::read(&path).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        // CRC catches it; nothing is committed.
        let result = Wal::read_committed(&path).unwrap();
        assert!(result.is_none());
    }
}
