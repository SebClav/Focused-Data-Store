//! `.fds` format version 2 (SPEC.md §6): one self-contained file holding
//! metadata plus typed records -- facts, procedures and knowledge, with
//! optional embeddings -- so an FDS database is a single portable artifact.
//!
//! Layout (little-endian):
//!   magic "FDS2" | version u32 | record_count u32 | meta_offset u64 |
//!   meta_length u32 | index_offset u64            (32-byte header)
//!   metadata (bincode DbMeta)
//!   records  (bincode Record, back to back)
//!   index    (record_count x (offset u64, length u32))

use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use crate::FdsError;

pub const MAGIC_V2: [u8; 4] = *b"FDS2";
pub const VERSION_V2: u32 = 2;
const HEADER_LEN: usize = 4 + 4 + 4 + 8 + 4 + 8;
const INDEX_ENTRY_LEN: usize = 8 + 4;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Fact,
    Procedure,
    Knowledge,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Fact => "fact",
            Kind::Procedure => "procedure",
            Kind::Knowledge => "knowledge",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "fact" => Some(Kind::Fact),
            "procedure" | "proc" => Some(Kind::Procedure),
            "knowledge" => Some(Kind::Knowledge),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Record {
    pub id: String,
    pub kind: Kind,
    pub collection: String,
    /// Canonical question (fact) or intent (procedure). None for knowledge.
    pub question: Option<String>,
    pub body: String,
    pub title: String,
    pub source: String,
    /// "human", "ingest", or "model:<name>".
    pub verified_by: String,
    /// YYYY-MM-DD.
    pub last_verified: String,
    pub tokens: u32,
    pub embedding: Option<Vec<f32>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct DbMeta {
    pub name: String,
    pub created: String,
    pub updated: String,
    /// Empty when the database has no embeddings.
    pub embedding_model: String,
    pub embedding_dims: u32,
}

/// Writes a complete database to a temporary file, then atomically replaces
/// `path` (SPEC.md §6.1): a reader never sees a half-written file.
pub fn write_db(path: &Path, meta: &DbMeta, records: &[Record]) -> Result<(), FdsError> {
    let meta_blob = bincode::serialize(meta)?;
    let mut blobs = Vec::with_capacity(records.len());
    for r in records {
        blobs.push(bincode::serialize(r)?);
    }

    let meta_offset = HEADER_LEN as u64;
    let records_start = meta_offset + meta_blob.len() as u64;
    let index_offset = records_start + blobs.iter().map(|b| b.len() as u64).sum::<u64>();

    let mut out = Vec::with_capacity(index_offset as usize + records.len() * INDEX_ENTRY_LEN);
    out.extend_from_slice(&MAGIC_V2);
    out.extend_from_slice(&VERSION_V2.to_le_bytes());
    out.extend_from_slice(&(records.len() as u32).to_le_bytes());
    out.extend_from_slice(&meta_offset.to_le_bytes());
    out.extend_from_slice(&(meta_blob.len() as u32).to_le_bytes());
    out.extend_from_slice(&index_offset.to_le_bytes());
    out.extend_from_slice(&meta_blob);

    let mut offsets = Vec::with_capacity(blobs.len());
    let mut cursor = records_start;
    for blob in &blobs {
        offsets.push((cursor, blob.len() as u32));
        out.extend_from_slice(blob);
        cursor += blob.len() as u64;
    }
    for (offset, length) in &offsets {
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
    }

    let tmp = path.with_extension("fds.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&out)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub struct DbReader {
    mmap: Mmap,
    count: u32,
    meta: DbMeta,
    index_offset: u64,
}

impl DbReader {
    pub fn open(path: &Path) -> Result<Self, FdsError> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };

        if mmap.len() < HEADER_LEN {
            return Err(FdsError::Truncated);
        }
        if mmap[0..4] != MAGIC_V2 {
            return Err(FdsError::BadMagic);
        }
        let version = u32::from_le_bytes(mmap[4..8].try_into().unwrap());
        if version != VERSION_V2 {
            return Err(FdsError::UnsupportedVersion(version));
        }
        let count = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        let meta_offset = u64::from_le_bytes(mmap[12..20].try_into().unwrap()) as usize;
        let meta_len = u32::from_le_bytes(mmap[20..24].try_into().unwrap()) as usize;
        let index_offset = u64::from_le_bytes(mmap[24..32].try_into().unwrap());

        if meta_offset + meta_len > mmap.len() {
            return Err(FdsError::Truncated);
        }
        let meta: DbMeta = bincode::deserialize(&mmap[meta_offset..meta_offset + meta_len])?;

        let index_end = index_offset as usize + count as usize * INDEX_ENTRY_LEN;
        if index_end > mmap.len() {
            return Err(FdsError::Truncated);
        }

        Ok(DbReader { mmap, count, meta, index_offset })
    }

    pub fn meta(&self) -> &DbMeta {
        &self.meta
    }

    pub fn len(&self) -> u32 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Reads one record without touching any other.
    pub fn get(&self, i: u32) -> Result<Record, FdsError> {
        if i >= self.count {
            return Err(FdsError::Truncated);
        }
        let at = self.index_offset as usize + i as usize * INDEX_ENTRY_LEN;
        let offset = u64::from_le_bytes(self.mmap[at..at + 8].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(self.mmap[at + 8..at + 12].try_into().unwrap()) as usize;
        if offset + length > self.mmap.len() {
            return Err(FdsError::Truncated);
        }
        Ok(bincode::deserialize(&self.mmap[offset..offset + length])?)
    }

    pub fn iter(&self) -> impl Iterator<Item = Result<Record, FdsError>> + '_ {
        (0..self.count).map(move |i| self.get(i))
    }
}

/// Loads a whole database into memory and releases the file. Use before
/// writing: on Windows a memory-mapped file cannot be replaced.
pub fn read_db(path: &Path) -> Result<(DbMeta, Vec<Record>), FdsError> {
    let reader = DbReader::open(path)?;
    let meta = reader.meta().clone();
    let records = reader.iter().collect::<Result<Vec<_>, _>>()?;
    drop(reader);
    Ok((meta, records))
}
