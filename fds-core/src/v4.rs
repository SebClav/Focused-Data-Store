//! `.fds` format version 4 (SPEC.md §6), the current one.
//!
//! Version 4 is written to be implemented without this code. Every byte of the
//! header, the index and the encoding of a record is specified; a record is a
//! CBOR map (RFC 8949), so any language reads it with a library it already
//! has; and every region carries a CRC-32, so damage is an error and never a
//! wrong answer.
//!
//! ```text
//! offset size  field
//!    0     8   magic            89 46 44 53 0D 0A 1A 0A   ("\x89FDS\r\n\x1a\n")
//!    8     2   format_major     u16 = 4
//!   10     2   format_minor     u16 = 0
//!   12     4   application_id   u32 (0: none)
//!   16     4   user_version     u32 (the application's own)
//!   20     4   record_count     u32
//!   24     8   meta_offset      u64
//!   32     4   meta_length      u32
//!   36     8   index_offset     u64
//!   44     4   meta_crc32       u32
//!   48     4   index_crc32      u32
//!   52     8   reserved         zero
//!   60     4   header_crc32     u32, of bytes 0..60
//! ```
//!
//! The header is followed by the metadata (a CBOR map), the records (CBOR
//! maps, back to back) and the index: per record, `offset u64`, `length u32`
//! and `crc32 u32` of that record. Every integer in the header and the index
//! is little-endian.
//!
//! Compatibility: a reader MUST refuse a different major; it MUST read any
//! minor of its major, ignoring what it does not know; and a writer MUST NOT
//! rewrite a file with a newer minor than its own, because rewriting would
//! drop what it cannot see. Versions 2 and 3 are still read, through their
//! frozen readers, and come back as version 4 records; saving writes version
//! 4, so a database migrates the first time it is saved.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::cbor::{Reader, Writer};
use crate::crc32::crc32;
use crate::{atomic, v2, v3, FdsError};

pub const MAGIC: [u8; 8] = [0x89, b'F', b'D', b'S', 0x0D, 0x0A, 0x1A, 0x0A];
pub const FORMAT_MAJOR: u16 = 4;
pub const FORMAT_MINOR: u16 = 0;
pub const HEADER_LEN: usize = 64;
pub const INDEX_ENTRY_LEN: usize = 16;

/// How a record is answered (SPEC §4) -- not what it is. What it is lives in
/// `Record::rtype`, which is free text.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A question and its verified answer, matched at stage 1.
    Fact,
    /// An intent and its steps, matched at stage 1.
    Procedure,
    /// Text found by retrieval at stage 2.
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
    /// What the record is, in the owner's words: "image", "command",
    /// "context", "recipe" -- anything. Empty means unspecified. FDS gives
    /// meaning to exactly one value, "context" (SPEC §4.4); every other type
    /// is the owner's to define.
    #[serde(rename = "type")]
    pub rtype: String,
    /// Free metadata: a file's hash, the model that described an image, a
    /// shell. Sorted, so equal records serialise to equal bytes.
    pub attrs: BTreeMap<String, String>,
    /// Canonical targets of links given explicitly, beyond any `[[link]]`
    /// written in the body. A target may not exist yet: links resolve when
    /// read, so a link can be written before the record it points to.
    pub links: Vec<String>,
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

impl From<v2::Kind> for Kind {
    fn from(k: v2::Kind) -> Self {
        match k {
            v2::Kind::Fact => Kind::Fact,
            v2::Kind::Procedure => Kind::Procedure,
            v2::Kind::Knowledge => Kind::Knowledge,
        }
    }
}

impl From<v2::Record> for Record {
    fn from(r: v2::Record) -> Self {
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
            rtype: String::new(),
            attrs: BTreeMap::new(),
            links: Vec::new(),
        }
    }
}

impl From<v2::DbMeta> for DbMeta {
    fn from(m: v2::DbMeta) -> Self {
        DbMeta {
            name: m.name,
            created: m.created,
            updated: m.updated,
            embedding_model: m.embedding_model,
            embedding_dims: m.embedding_dims,
        }
    }
}

// ── encoding of a record and of the metadata ─────────────────────────────

fn enc(f: impl FnOnce(&mut Writer)) -> Vec<u8> {
    let mut w = Writer::new();
    f(&mut w);
    w.out
}

fn pair(key: &str, value: Vec<u8>) -> (Vec<u8>, Vec<u8>) {
    (enc(|w| w.text(key)), value)
}

/// A map from already-encoded pairs, keys in the bytewise order of their
/// encodings (RFC 8949 §4.2.1): the same record is the same bytes everywhere.
fn map_of(mut entries: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut w = Writer::new();
    w.map(entries.len());
    for (k, v) in &entries {
        w.raw(k);
        w.raw(v);
    }
    w.out
}

fn f32_le(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A record as the CBOR map SPEC.md §6.4 defines.
pub fn encode_record(r: &Record) -> Vec<u8> {
    let mut e = vec![
        pair("id", enc(|w| w.text(&r.id))),
        pair("kind", enc(|w| w.text(r.kind.as_str()))),
        pair("collection", enc(|w| w.text(&r.collection))),
        pair("body", enc(|w| w.text(&r.body))),
        pair("title", enc(|w| w.text(&r.title))),
        pair("source", enc(|w| w.text(&r.source))),
        pair("verified_by", enc(|w| w.text(&r.verified_by))),
        pair("last_verified", enc(|w| w.text(&r.last_verified))),
        pair("tokens", enc(|w| w.uint(r.tokens as u64))),
    ];
    if let Some(q) = &r.question {
        e.push(pair("question", enc(|w| w.text(q))));
    }
    if let Some(v) = &r.embedding {
        e.push(pair("embedding", enc(|w| w.bytes(&f32_le(v)))));
    }
    if !r.rtype.is_empty() {
        e.push(pair("type", enc(|w| w.text(&r.rtype))));
    }
    if !r.attrs.is_empty() {
        let entries = r
            .attrs
            .iter()
            .map(|(k, v)| (enc(|w| w.text(k)), enc(|w| w.text(v))))
            .collect();
        e.push(pair("attrs", map_of(entries)));
    }
    if !r.links.is_empty() {
        e.push(pair(
            "links",
            enc(|w| {
                w.array(r.links.len());
                for l in &r.links {
                    w.text(l);
                }
            }),
        ));
    }
    map_of(e)
}

pub fn encode_meta(m: &DbMeta) -> Vec<u8> {
    map_of(vec![
        pair("name", enc(|w| w.text(&m.name))),
        pair("created", enc(|w| w.text(&m.created))),
        pair("updated", enc(|w| w.text(&m.updated))),
        pair("embedding_model", enc(|w| w.text(&m.embedding_model))),
        pair("embedding_dims", enc(|w| w.uint(m.embedding_dims as u64))),
    ])
}

fn once<T>(slot: &mut Option<T>, value: T, key: &str) -> Result<(), FdsError> {
    if slot.is_some() {
        return Err(FdsError::Malformed(format!("key `{key}` appears twice")));
    }
    *slot = Some(value);
    Ok(())
}

fn missing(key: &str) -> FdsError {
    FdsError::Malformed(format!("required key `{key}` is missing"))
}

fn u32_of(n: u64, what: &str) -> Result<u32, FdsError> {
    u32::try_from(n).map_err(|_| FdsError::Malformed(format!("{what} does not fit in 32 bits")))
}

/// Reads a record. Keys it does not know are skipped, whatever their type;
/// optional keys that are absent take their empty value.
pub fn decode_record(buf: &[u8]) -> Result<Record, FdsError> {
    let mut r = Reader::new(buf);
    let n = r.map_len()?;
    let (mut id, mut kind, mut collection, mut question) = (None, None, None, None);
    let (mut body, mut title, mut source, mut verified_by, mut last_verified) = (None, None, None, None, None);
    let (mut tokens, mut embedding, mut rtype, mut attrs, mut links) = (None, None, None, None, None);

    for _ in 0..n {
        match r.text_ref()? {
            "id" => once(&mut id, r.text()?, "id")?,
            "kind" => once(&mut kind, r.text()?, "kind")?,
            "collection" => once(&mut collection, r.text()?, "collection")?,
            "question" => once(&mut question, r.text_or_null()?, "question")?,
            "body" => once(&mut body, r.text()?, "body")?,
            "title" => once(&mut title, r.text()?, "title")?,
            "source" => once(&mut source, r.text()?, "source")?,
            "verified_by" => once(&mut verified_by, r.text()?, "verified_by")?,
            "last_verified" => once(&mut last_verified, r.text()?, "last_verified")?,
            "tokens" => once(&mut tokens, u32_of(r.uint()?, "tokens")?, "tokens")?,
            "embedding" => {
                let raw = r.bytes()?;
                if raw.len() % 4 != 0 {
                    return Err(FdsError::Malformed("embedding is not a whole number of f32".into()));
                }
                let v: Vec<f32> = raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
                once(&mut embedding, v, "embedding")?
            }
            "type" => once(&mut rtype, r.text()?, "type")?,
            "attrs" => {
                let m = r.map_len()?;
                let mut map = BTreeMap::new();
                for _ in 0..m {
                    let k = r.text()?;
                    let v = r.text()?;
                    if map.insert(k, v).is_some() {
                        return Err(FdsError::Malformed("an attribute appears twice".into()));
                    }
                }
                once(&mut attrs, map, "attrs")?
            }
            "links" => {
                let m = r.array_len()?;
                let mut v = Vec::new();
                for _ in 0..m {
                    v.push(r.text()?);
                }
                once(&mut links, v, "links")?
            }
            _ => r.skip()?,
        }
    }
    if !r.is_done() {
        return Err(FdsError::Malformed("bytes after the end of a record".into()));
    }

    let kind = match kind.ok_or_else(|| missing("kind"))?.as_str() {
        "fact" => Kind::Fact,
        "procedure" => Kind::Procedure,
        "knowledge" => Kind::Knowledge,
        other => return Err(FdsError::Malformed(format!("unknown kind `{other}`"))),
    };
    Ok(Record {
        id: id.ok_or_else(|| missing("id"))?,
        kind,
        collection: collection.ok_or_else(|| missing("collection"))?,
        question: question.unwrap_or(None),
        body: body.ok_or_else(|| missing("body"))?,
        title: title.ok_or_else(|| missing("title"))?,
        source: source.ok_or_else(|| missing("source"))?,
        verified_by: verified_by.ok_or_else(|| missing("verified_by"))?,
        last_verified: last_verified.ok_or_else(|| missing("last_verified"))?,
        tokens: tokens.ok_or_else(|| missing("tokens"))?,
        embedding,
        rtype: rtype.unwrap_or_default(),
        attrs: attrs.unwrap_or_default(),
        links: links.unwrap_or_default(),
    })
}

pub fn decode_meta(buf: &[u8]) -> Result<DbMeta, FdsError> {
    let mut r = Reader::new(buf);
    let n = r.map_len()?;
    let mut m = DbMeta::default();
    for _ in 0..n {
        match r.text_ref()? {
            "name" => m.name = r.text()?,
            "created" => m.created = r.text()?,
            "updated" => m.updated = r.text()?,
            "embedding_model" => m.embedding_model = r.text()?,
            "embedding_dims" => m.embedding_dims = u32_of(r.uint()?, "embedding_dims")?,
            _ => r.skip()?,
        }
    }
    if !r.is_done() {
        return Err(FdsError::Malformed("bytes after the end of the metadata".into()));
    }
    Ok(m)
}

// ── the file ─────────────────────────────────────────────────────────────

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// What the first bytes of a file say about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub major: u16,
    pub minor: u16,
    pub application_id: u32,
    pub user_version: u32,
    pub record_count: u32,
    pub meta_offset: u64,
    pub meta_length: u32,
    pub index_offset: u64,
    pub meta_crc32: u32,
    pub index_crc32: u32,
}

fn parse_header(buf: &[u8]) -> Result<Header, FdsError> {
    // Checks 1 to 4 of SPEC.md §6.6, in that order.
    let m = buf.len().min(MAGIC.len());
    if buf[..m] != MAGIC[..m] {
        return Err(FdsError::BadMagic);
    }
    if buf.len() < 10 {
        return Err(FdsError::Truncated);
    }
    let major = u16_at(buf, 8);
    if major != FORMAT_MAJOR {
        return Err(FdsError::UnsupportedVersion(major as u32));
    }
    if buf.len() < HEADER_LEN {
        return Err(FdsError::Truncated);
    }
    if crc32(&buf[..60]) != u32_at(buf, 60) {
        return Err(FdsError::BadChecksum("header"));
    }
    // Bytes 52..60 are reserved: a writer puts zero there, a reader does not
    // look, so a later minor version can give them a meaning.
    Ok(Header {
        major,
        minor: u16_at(buf, 10),
        application_id: u32_at(buf, 12),
        user_version: u32_at(buf, 16),
        record_count: u32_at(buf, 20),
        meta_offset: u64_at(buf, 24),
        meta_length: u32_at(buf, 32),
        index_offset: u64_at(buf, 36),
        meta_crc32: u32_at(buf, 44),
        index_crc32: u32_at(buf, 48),
    })
}

/// Which format generation a file is: 4, 3 or 2 (1 is recognised, not read).
///
/// A file whose first bytes are a prefix of the magic (an empty file, or a
/// copy that stopped early) is truncated; anything else that is not a known
/// magic is not an FDS file (SPEC.md §6.6, check 1).
pub fn format_version(path: &Path) -> Result<u32, FdsError> {
    let mut head = Vec::with_capacity(10);
    File::open(path)?.take(10).read_to_end(&mut head)?;
    let m = head.len().min(MAGIC.len());
    if head[..m] == MAGIC[..m] {
        if head.len() < 10 {
            return Err(FdsError::Truncated);
        }
        return match u16_at(&head, 8) {
            major @ 0..=3 => Err(FdsError::UnsupportedVersion(major as u32)),
            major => Ok(major as u32),
        };
    }
    if head.len() >= 4 {
        match &head[..4] {
            b"FDS3" => return Ok(3),
            b"FDS2" => return Ok(2),
            b"FDS1" => return Ok(1),
            _ => {}
        }
    }
    Err(FdsError::BadMagic)
}

/// A file written by a newer minor must not be rewritten by an older writer:
/// the rewrite would drop what the writer cannot see (SPEC.md §6.2).
fn refuse_newer(path: &Path) -> Result<(), FdsError> {
    let mut head = [0u8; 12];
    if let Ok(mut f) = File::open(path) {
        if f.read_exact(&mut head).is_ok() && head[..8] == MAGIC {
            let (major, minor) = (u16_at(&head, 8), u16_at(&head, 10));
            if major > FORMAT_MAJOR || (major == FORMAT_MAJOR && minor > FORMAT_MINOR) {
                return Err(FdsError::NewerFormat { major, minor });
            }
        }
    }
    Ok(())
}

/// Builds a complete version 4 file.
pub fn encode_db(meta: &DbMeta, records: &[Record]) -> Result<Vec<u8>, FdsError> {
    let too_big = |what: &str| FdsError::Malformed(format!("{what} is too large for the format"));
    if records.len() > u32::MAX as usize {
        return Err(too_big("the record count"));
    }
    let meta_blob = encode_meta(meta);
    let blobs: Vec<Vec<u8>> = records.iter().map(encode_record).collect();
    if blobs.iter().any(|b| b.len() > u32::MAX as usize) || meta_blob.len() > u32::MAX as usize {
        return Err(too_big("a record"));
    }

    let meta_offset = HEADER_LEN as u64;
    let records_start = meta_offset + meta_blob.len() as u64;
    let index_offset = records_start + blobs.iter().map(|b| b.len() as u64).sum::<u64>();

    let mut index = Vec::with_capacity(blobs.len() * INDEX_ENTRY_LEN);
    let mut cursor = records_start;
    for b in &blobs {
        index.extend_from_slice(&cursor.to_le_bytes());
        index.extend_from_slice(&(b.len() as u32).to_le_bytes());
        index.extend_from_slice(&crc32(b).to_le_bytes());
        cursor += b.len() as u64;
    }

    let mut h = Vec::with_capacity(HEADER_LEN);
    h.extend_from_slice(&MAGIC);
    h.extend_from_slice(&FORMAT_MAJOR.to_le_bytes());
    h.extend_from_slice(&FORMAT_MINOR.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // application_id
    h.extend_from_slice(&0u32.to_le_bytes()); // user_version
    h.extend_from_slice(&(records.len() as u32).to_le_bytes());
    h.extend_from_slice(&meta_offset.to_le_bytes());
    h.extend_from_slice(&(meta_blob.len() as u32).to_le_bytes());
    h.extend_from_slice(&index_offset.to_le_bytes());
    h.extend_from_slice(&crc32(&meta_blob).to_le_bytes());
    h.extend_from_slice(&crc32(&index).to_le_bytes());
    h.extend_from_slice(&[0u8; 8]); // reserved
    let header_crc = crc32(&h);
    h.extend_from_slice(&header_crc.to_le_bytes());
    debug_assert_eq!(h.len(), HEADER_LEN);

    let mut out = Vec::with_capacity(index_offset as usize + index.len());
    out.extend_from_slice(&h);
    out.extend_from_slice(&meta_blob);
    for b in &blobs {
        out.extend_from_slice(b);
    }
    out.extend_from_slice(&index);
    Ok(out)
}

/// Writes a complete database to a temporary file, then atomically replaces
/// `path` (SPEC.md §6.1): a reader never sees a half-written file.
pub fn write_db(path: &Path, meta: &DbMeta, records: &[Record]) -> Result<(), FdsError> {
    refuse_newer(path)?;
    atomic::write(path, &encode_db(meta, records)?)
}

struct V4 {
    mmap: Mmap,
    header: Header,
    meta: DbMeta,
}

enum Inner {
    V4(V4),
    V3(v3::Frozen),
    V2 { reader: v2::DbReader, meta: DbMeta },
}

/// Reads version 4 natively, and versions 3 and 2 through their frozen readers.
pub struct DbReader {
    inner: Inner,
}

impl DbReader {
    pub fn open(path: &Path) -> Result<Self, FdsError> {
        match format_version(path)? {
            4 => {}
            3 => return Ok(DbReader { inner: Inner::V3(v3::Frozen::open(path)?) }),
            2 => {
                let reader = v2::DbReader::open(path)?;
                let meta = reader.meta().clone().into();
                return Ok(DbReader { inner: Inner::V2 { reader, meta } });
            }
            1 => return Err(FdsError::BadMagic),
            v => return Err(FdsError::UnsupportedVersion(v)),
        }

        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let header = parse_header(&mmap)?;

        let len = mmap.len() as u64;
        let meta_end = header.meta_offset.checked_add(header.meta_length as u64).ok_or(FdsError::Truncated)?;
        if meta_end > len {
            return Err(FdsError::Truncated);
        }
        if header.meta_offset < HEADER_LEN as u64 {
            return Err(FdsError::Malformed("the metadata overlaps the header".into()));
        }
        let meta_bytes = &mmap[header.meta_offset as usize..meta_end as usize];
        if crc32(meta_bytes) != header.meta_crc32 {
            return Err(FdsError::BadChecksum("metadata"));
        }
        let index_len = (header.record_count as u64) * INDEX_ENTRY_LEN as u64;
        let index_end = header.index_offset.checked_add(index_len).ok_or(FdsError::Truncated)?;
        if index_end > len {
            return Err(FdsError::Truncated);
        }
        if header.index_offset < meta_end {
            return Err(FdsError::Malformed("the index overlaps the metadata".into()));
        }
        if crc32(&mmap[header.index_offset as usize..index_end as usize]) != header.index_crc32 {
            return Err(FdsError::BadChecksum("index"));
        }
        let meta = decode_meta(meta_bytes)?;
        Ok(DbReader { inner: Inner::V4(V4 { mmap, header, meta }) })
    }

    /// The format generation of the file on disk: 4, or 3 or 2 until it is saved.
    pub fn version(&self) -> u32 {
        match self.inner {
            Inner::V4(_) => 4,
            Inner::V3(_) => 3,
            Inner::V2 { .. } => 2,
        }
    }

    /// The minor version of a version 4 file; 0 for the older generations.
    pub fn minor(&self) -> u16 {
        match &self.inner {
            Inner::V4(v) => v.header.minor,
            _ => 0,
        }
    }

    pub fn meta(&self) -> &DbMeta {
        match &self.inner {
            Inner::V4(v) => &v.meta,
            Inner::V3(f) => f.meta(),
            Inner::V2 { meta, .. } => meta,
        }
    }

    pub fn len(&self) -> u32 {
        match &self.inner {
            Inner::V4(v) => v.header.record_count,
            Inner::V3(f) => f.len(),
            Inner::V2 { reader, .. } => reader.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads one record without touching any other, and checks its CRC.
    pub fn get(&self, i: u32) -> Result<Record, FdsError> {
        match &self.inner {
            Inner::V2 { reader, .. } => reader.get(i).map(Record::from),
            Inner::V3(f) => f.get(i),
            Inner::V4(v) => {
                if i >= v.header.record_count {
                    return Err(FdsError::Truncated);
                }
                let at = v.header.index_offset as usize + i as usize * INDEX_ENTRY_LEN;
                let offset = u64_at(&v.mmap, at);
                let length = u32_at(&v.mmap, at + 8) as u64;
                let crc = u32_at(&v.mmap, at + 12);
                let records_start = v.header.meta_offset + v.header.meta_length as u64;
                let end = offset.checked_add(length).ok_or(FdsError::Truncated)?;
                if offset < records_start || end > v.header.index_offset {
                    return Err(FdsError::Malformed("a record lies outside the records region".into()));
                }
                let bytes = &v.mmap[offset as usize..end as usize];
                if crc32(bytes) != crc {
                    return Err(FdsError::BadChecksum("record"));
                }
                decode_record(bytes)
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = Result<Record, FdsError>> + '_ {
        (0..self.len()).map(move |i| self.get(i))
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
