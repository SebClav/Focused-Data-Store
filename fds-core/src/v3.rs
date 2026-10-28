//! `.fds` format version 3, frozen: read, never again written by the tool.
//!
//! Version 3 was version 2 plus the three fields that make a database
//! moldable without new code -- a free `type`, free `attrs`, and `links`
//! between records. Its header:
//!
//!   magic "FDS3" | version u32 | record_count u32 | meta_offset u64 |
//!   meta_length u32 | index_offset u64            (32-byte header)
//!   metadata (bincode DbMeta)
//!   records  (bincode Record, back to back)
//!   index    (record_count x (offset u64, length u32))
//!
//! Records are bincode, which is positional and tied to Rust's types: that is
//! why version 4 (SPEC.md §6) replaced it with CBOR. This module keeps its own
//! copies of the v3 types, so a change to the current types can never change
//! how a v3 file is read. `write_db` is kept only so migration can be tested.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::v4::{DbMeta, Kind, Record};
use crate::{atomic, FdsError};

pub const MAGIC: [u8; 4] = *b"FDS3";
const VERSION: u32 = 3;
const HEADER_LEN: usize = 4 + 4 + 4 + 8 + 4 + 8;
const INDEX_ENTRY_LEN: usize = 8 + 4;

#[derive(Serialize, Deserialize, Clone, Copy)]
enum KindV3 {
    Fact,
    Procedure,
    Knowledge,
}

#[derive(Serialize, Deserialize)]
struct RecordV3 {
    id: String,
    kind: KindV3,
    collection: String,
    question: Option<String>,
    body: String,
    title: String,
    source: String,
    verified_by: String,
    last_verified: String,
    tokens: u32,
    embedding: Option<Vec<f32>>,
    rtype: String,
    attrs: BTreeMap<String, String>,
    links: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct DbMetaV3 {
    name: String,
    created: String,
    updated: String,
    embedding_model: String,
    embedding_dims: u32,
}

impl From<KindV3> for Kind {
    fn from(k: KindV3) -> Self {
        match k {
            KindV3::Fact => Kind::Fact,
            KindV3::Procedure => Kind::Procedure,
            KindV3::Knowledge => Kind::Knowledge,
        }
    }
}

impl From<Kind> for KindV3 {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Fact => KindV3::Fact,
            Kind::Procedure => KindV3::Procedure,
            Kind::Knowledge => KindV3::Knowledge,
        }
    }
}

impl From<RecordV3> for Record {
    fn from(r: RecordV3) -> Self {
        Record {
            id: r.id,
            kind: r.kind.into(),
            collection: r.collection,
            question: r.question,
            body: r.body,
            title: r.title,
            source: r.source,
            verified_by: r.verified_by,
            last_verified: r.last_verified,
            tokens: r.tokens,
            embedding: r.embedding,
            rtype: r.rtype,
            attrs: r.attrs,
            links: r.links,
        }
    }
}

impl From<&Record> for RecordV3 {
    fn from(r: &Record) -> Self {
        RecordV3 {
            id: r.id.clone(),
            kind: r.kind.into(),
            collection: r.collection.clone(),
            question: r.question.clone(),
            body: r.body.clone(),
            title: r.title.clone(),
            source: r.source.clone(),
            verified_by: r.verified_by.clone(),
            last_verified: r.last_verified.clone(),
            tokens: r.tokens,
            embedding: r.embedding.clone(),
            rtype: r.rtype.clone(),
            attrs: r.attrs.clone(),
            links: r.links.clone(),
        }
    }
}

impl From<DbMetaV3> for DbMeta {
    fn from(m: DbMetaV3) -> Self {
        DbMeta {
            name: m.name,
            created: m.created,
            updated: m.updated,
            embedding_model: m.embedding_model,
            embedding_dims: m.embedding_dims,
        }
    }
}

/// The reader behind `v4::DbReader` for version 3 files.
pub(crate) struct Frozen {
    mmap: Mmap,
    count: u32,
    meta: DbMeta,
    index_offset: u64,
}

impl Frozen {
    pub(crate) fn open(path: &Path) -> Result<Self, FdsError> {
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
        let count = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        let meta_offset = u64::from_le_bytes(mmap[12..20].try_into().unwrap()) as usize;
        let meta_len = u32::from_le_bytes(mmap[20..24].try_into().unwrap()) as usize;
        let index_offset = u64::from_le_bytes(mmap[24..32].try_into().unwrap());
        if meta_offset.saturating_add(meta_len) > mmap.len() {
            return Err(FdsError::Truncated);
        }
        let meta: DbMetaV3 = bincode::deserialize(&mmap[meta_offset..meta_offset + meta_len])?;
        let index_end = (index_offset as usize).saturating_add(count as usize * INDEX_ENTRY_LEN);
        if index_end > mmap.len() {
            return Err(FdsError::Truncated);
        }
        Ok(Frozen { mmap, count, meta: meta.into(), index_offset })
    }

    pub(crate) fn meta(&self) -> &DbMeta {
        &self.meta
    }

    pub(crate) fn len(&self) -> u32 {
        self.count
    }

    pub(crate) fn get(&self, i: u32) -> Result<Record, FdsError> {
        if i >= self.count {
            return Err(FdsError::Truncated);
        }
        let at = self.index_offset as usize + i as usize * INDEX_ENTRY_LEN;
        let offset = u64::from_le_bytes(self.mmap[at..at + 8].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(self.mmap[at + 8..at + 12].try_into().unwrap()) as usize;
        if offset.saturating_add(length) > self.mmap.len() {
            return Err(FdsError::Truncated);
        }
        let r: RecordV3 = bincode::deserialize(&self.mmap[offset..offset + length])?;
        Ok(r.into())
    }
}

/// Writes a version 3 file. The tool never calls this: it exists so the
/// migration from 3 to 4 can be tested against a file a real v3 writer made.
pub fn write_db(path: &Path, meta: &DbMeta, records: &[Record]) -> Result<(), FdsError> {
    let meta_blob = bincode::serialize(&DbMetaV3 {
        name: meta.name.clone(),
        created: meta.created.clone(),
        updated: meta.updated.clone(),
        embedding_model: meta.embedding_model.clone(),
        embedding_dims: meta.embedding_dims,
    })?;
    let mut blobs = Vec::with_capacity(records.len());
    for r in records {
        blobs.push(bincode::serialize(&RecordV3::from(r))?);
    }

    let meta_offset = HEADER_LEN as u64;
    let records_start = meta_offset + meta_blob.len() as u64;
    let index_offset = records_start + blobs.iter().map(|b| b.len() as u64).sum::<u64>();

    let mut out = Vec::with_capacity(index_offset as usize + records.len() * INDEX_ENTRY_LEN);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
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
    atomic::write(path, &out)
}
