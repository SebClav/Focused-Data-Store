//! FDS Milestone 1: minimal on-disk format proving the mmap read/write
//! round-trip works before adding compression, checksums, dedup, or a
//! schema/catalog (FDS_DESIGN_SPEC.md §4's full section list comes in
//! later milestones -- this proves the foundation first).
//!
//! Layout:
//!   [Header: 20 bytes]  magic(4) + version(u32) + chunk_count(u32) + index_offset(u64)
//!   [Chunk blobs]       each chunk bincode-serialized, back to back
//!   [Index]             chunk_count entries of (offset: u64, length: u32), 12 bytes each
//!
//! Individual chunks are randomly addressable via the index without
//! deserializing the whole file -- the actual point of using mmap at all.

use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

pub mod atomic;
pub mod cbor;
pub mod crc32;
pub mod v2;
pub mod v3;
pub mod v4;

pub const MAGIC: [u8; 4] = *b"FDS1";
pub const VERSION: u32 = 1;
const HEADER_LEN: usize = 4 + 4 + 4 + 8;
const INDEX_ENTRY_LEN: usize = 8 + 4;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ChunkRecord {
    pub chunk_id: String,
    pub collection: String,
    pub source: String,
    pub section: String,
    pub heading_path: String,
    pub title: String,
    pub body: String,
    pub token_count: u32,
}

#[derive(Debug)]
pub enum FdsError {
    Io(io::Error),
    Bincode(bincode::Error),
    BadMagic,
    UnsupportedVersion(u32),
    Truncated,
    /// A region's CRC-32 does not match: the file is damaged (version 4).
    BadChecksum(&'static str),
    /// The file was written by a newer minor version than this writer knows;
    /// it can be read, not rewritten (SPEC.md §6.2).
    NewerFormat { major: u16, minor: u16 },
    /// The bytes are not what the format says: an unknown kind, a missing
    /// required key, text that is not UTF-8.
    Malformed(String),
}

impl From<io::Error> for FdsError {
    fn from(e: io::Error) -> Self {
        FdsError::Io(e)
    }
}
impl From<bincode::Error> for FdsError {
    fn from(e: bincode::Error) -> Self {
        FdsError::Bincode(e)
    }
}

/// Writes chunks to a .fds file: header + concatenated chunk blobs + index.
/// Builds the whole file in memory first (input is small; streaming with
/// seek-back-to-patch-header is a fine follow-up, not needed yet).
pub fn write_fds(path: &Path, chunks: &[ChunkRecord]) -> Result<(), FdsError> {
    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(chunks.len());
    for c in chunks {
        blobs.push(bincode::serialize(c)?);
    }

    let index_offset = HEADER_LEN as u64
        + blobs.iter().map(|b| b.len() as u64).sum::<u64>();

    let mut out = Vec::with_capacity(index_offset as usize + chunks.len() * INDEX_ENTRY_LEN);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    out.extend_from_slice(&index_offset.to_le_bytes());

    let mut offsets = Vec::with_capacity(blobs.len());
    let mut cursor = HEADER_LEN as u64;
    for blob in &blobs {
        offsets.push((cursor, blob.len() as u32));
        out.extend_from_slice(blob);
        cursor += blob.len() as u64;
    }

    for (offset, length) in &offsets {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
    }

    let mut file = File::create(path)?;
    file.write_all(&out)?;
    Ok(())
}

pub struct FdsReader {
    mmap: Mmap,
    chunk_count: u32,
    index_offset: u64,
}

impl FdsReader {
    pub fn open(path: &Path) -> Result<Self, FdsError> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };

        if mmap.len() < HEADER_LEN {
            return Err(FdsError::Truncated);
        }
        if mmap[0..4] != MAGIC {
            return Err(FdsError::BadMagic);
        }
        let version = u32::from_le_bytes(mmap[4..8].try_into().unwrap());
        if version != VERSION {
            return Err(FdsError::UnsupportedVersion(version));
        }
        let chunk_count = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        let index_offset = u64::from_le_bytes(mmap[12..20].try_into().unwrap());

        Ok(FdsReader { mmap, chunk_count, index_offset })
    }

    pub fn len(&self) -> u32 {
        self.chunk_count
    }

    fn index_entry(&self, i: u32) -> Result<(u64, u32), FdsError> {
        if i >= self.chunk_count {
            return Err(FdsError::Truncated);
        }
        let start = self.index_offset as usize + (i as usize) * INDEX_ENTRY_LEN;
        let end = start + INDEX_ENTRY_LEN;
        if end > self.mmap.len() {
            return Err(FdsError::Truncated);
        }
        let offset = u64::from_le_bytes(self.mmap[start..start + 8].try_into().unwrap());
        let length = u32::from_le_bytes(self.mmap[start + 8..start + 12].try_into().unwrap());
        Ok((offset, length))
    }

    /// Random-access read of a single chunk by index -- does not touch or
    /// deserialize any other chunk in the file.
    pub fn get(&self, i: u32) -> Result<ChunkRecord, FdsError> {
        let (offset, length) = self.index_entry(i)?;
        let start = offset as usize;
        let end = start + length as usize;
        if end > self.mmap.len() {
            return Err(FdsError::Truncated);
        }
        let record: ChunkRecord = bincode::deserialize(&self.mmap[start..end])?;
        Ok(record)
    }

    pub fn iter(&self) -> impl Iterator<Item = Result<ChunkRecord, FdsError>> + '_ {
        (0..self.chunk_count).map(move |i| self.get(i))
    }
}
