//! Format version 4 (SPEC.md §6): what it writes is what the spec says, damage
//! is an error and not a wrong answer, an older reader copes with a newer
//! minor, and versions 2 and 3 migrate without loss.

use std::collections::BTreeMap;
use std::path::PathBuf;

use fds::cbor::Writer;
use fds::crc32::crc32;
use fds::v4::{
    decode_record, encode_db, encode_record, format_version, read_db, write_db, DbMeta, DbReader, Kind, Record,
    HEADER_LEN, INDEX_ENTRY_LEN,
};
use fds::FdsError;

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("fds-v4-{name}-{}.fds", std::process::id()))
}

fn image_record() -> Record {
    let mut attrs = BTreeMap::new();
    attrs.insert("file".into(), "fotos/router.jpg".into());
    attrs.insert("described_by".into(), "moondream".into());
    Record {
        id: "a1b2c3d4e5f60718".into(),
        kind: Kind::Knowledge,
        collection: "soporte".into(),
        question: None,
        body: "Panel trasero: 4 LAN amarillos, 1 WAN azul. Ver [[reiniciar el router]].".into(),
        title: "Panel trasero del router".into(),
        source: "fotos/router.jpg".into(),
        verified_by: "model:moondream".into(),
        last_verified: "2026-09-25".into(),
        tokens: 12,
        embedding: Some(vec![0.25, -0.5, 1.0]),
        rtype: "image".into(),
        attrs,
        links: vec!["ip de administracion del router".into()],
    }
}

fn fact(n: u32) -> Record {
    Record {
        id: format!("{n:016x}"),
        kind: Kind::Fact,
        collection: "default".into(),
        question: Some(format!("question {n}")),
        body: format!("answer {n}"),
        title: format!("question {n}"),
        source: "test".into(),
        verified_by: "human".into(),
        last_verified: "2026-10-19".into(),
        tokens: 2,
        embedding: None,
        rtype: String::new(),
        attrs: BTreeMap::new(),
        links: Vec::new(),
    }
}

#[test]
fn version_4_round_trips_every_field() {
    let path = scratch("roundtrip");
    let meta = DbMeta { name: "soporte".into(), created: "2026-09-25".into(), updated: "2026-10-19".into(), embedding_model: "all-minilm".into(), embedding_dims: 3 };
    let rec = image_record();
    write_db(&path, &meta, std::slice::from_ref(&rec)).unwrap();

    assert_eq!(format_version(&path).unwrap(), 4);
    let reader = DbReader::open(&path).unwrap();
    assert_eq!((reader.version(), reader.minor(), reader.len()), (4, 0, 1));
    assert_eq!(reader.get(0).unwrap(), rec);
    drop(reader);

    let (m, recs) = read_db(&path).unwrap();
    assert_eq!(m, meta);
    assert_eq!(recs, vec![rec]);
    let _ = std::fs::remove_file(path);
}

#[test]
fn an_empty_database_and_an_empty_text_survive() {
    let path = scratch("empty");
    write_db(&path, &DbMeta::default(), &[]).unwrap();
    assert_eq!(read_db(&path).unwrap(), (DbMeta::default(), vec![]));
    let mut r = fact(1);
    r.question = Some(String::new());
    r.embedding = Some(vec![]);
    write_db(&path, &DbMeta::default(), std::slice::from_ref(&r)).unwrap();
    assert_eq!(read_db(&path).unwrap().1, vec![r], "Some(\"\") and Some([]) are not None");
    let _ = std::fs::remove_file(path);
}

/// The bytes SPEC.md §6.4 says this record must be, written out by hand from
/// RFC 8949: nine keys, ordered by their encodings (shorter first, then
/// bytewise), definite lengths, shortest heads.
#[test]
fn a_record_is_exactly_the_bytes_the_spec_describes() {
    let r = Record {
        id: "a".into(),
        kind: Kind::Fact,
        collection: "c".into(),
        question: None,
        body: "b".into(),
        title: "t".into(),
        source: "s".into(),
        verified_by: "h".into(),
        last_verified: "d".into(),
        tokens: 1,
        embedding: None,
        rtype: String::new(),
        attrs: BTreeMap::new(),
        links: Vec::new(),
    };
    let expected: Vec<u8> = [
        &[0xa9u8][..],                                                       // map(9)
        &[0x62, b'i', b'd', 0x61, b'a'],                                     // "id": "a"
        &[0x64, b'b', b'o', b'd', b'y', 0x61, b'b'],                         // "body": "b"
        &[0x64, b'k', b'i', b'n', b'd', 0x64, b'f', b'a', b'c', b't'],       // "kind": "fact"
        &[0x65, b't', b'i', b't', b'l', b'e', 0x61, b't'],                   // "title": "t"
        &[0x66, b's', b'o', b'u', b'r', b'c', b'e', 0x61, b's'],             // "source": "s"
        &[0x66, b't', b'o', b'k', b'e', b'n', b's', 0x01],                   // "tokens": 1
        &[0x6a, b'c', b'o', b'l', b'l', b'e', b'c', b't', b'i', b'o', b'n', 0x61, b'c'], // "collection": "c"
        &[0x6b, b'v', b'e', b'r', b'i', b'f', b'i', b'e', b'd', b'_', b'b', b'y', 0x61, b'h'], // "verified_by": "h"
        &[0x6d, b'l', b'a', b's', b't', b'_', b'v', b'e', b'r', b'i', b'f', b'i', b'e', b'd', 0x61, b'd'], // "last_verified": "d"
    ]
    .concat();
    assert_eq!(encode_record(&r), expected);
    assert_eq!(decode_record(&expected).unwrap(), r);
}

#[test]
fn equal_records_are_equal_bytes_whatever_order_attrs_were_added() {
    let mut a = image_record();
    let mut b = image_record();
    a.attrs = [("x", "1"), ("yy", "2"), ("a", "3")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
    b.attrs = [("a", "3"), ("x", "1"), ("yy", "2")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
    assert_eq!(encode_record(&a), encode_record(&b));
}

#[test]
fn the_header_is_what_the_spec_says() {
    let bytes = encode_db(&DbMeta { name: "x".into(), ..Default::default() }, &[fact(1), fact(2)]).unwrap();
    assert_eq!(&bytes[..8], &[0x89, b'F', b'D', b'S', 0x0D, 0x0A, 0x1A, 0x0A]);
    let le16 = |at: usize| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
    let le32 = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let le64 = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    assert_eq!((le16(8), le16(10)), (4, 0), "major, minor");
    assert_eq!((le32(12), le32(16)), (0, 0), "application_id, user_version");
    assert_eq!(le32(20), 2, "record_count");
    assert_eq!(le64(24), HEADER_LEN as u64, "meta_offset: the metadata follows the header");
    assert_eq!(le64(36) as usize + 2 * INDEX_ENTRY_LEN, bytes.len(), "the index is the last thing in the file");
    assert_eq!(&bytes[52..60], &[0u8; 8], "reserved");
    assert_eq!(le32(60), crc32(&bytes[..60]), "header_crc32 covers bytes 0..60");
    assert_eq!(le32(44), crc32(&bytes[le64(24) as usize..(le64(24) + le32(32) as u64) as usize]), "meta_crc32");
}

// ── damage is an error ───────────────────────────────────────────────────

fn file_of(records: &[Record]) -> Vec<u8> {
    encode_db(&DbMeta { name: "damage".into(), ..Default::default() }, records).unwrap()
}

fn open_bytes(name: &str, bytes: &[u8]) -> Result<(DbMeta, Vec<Record>), FdsError> {
    let path = scratch(name);
    std::fs::write(&path, bytes).unwrap();
    let r = read_db(&path);
    let _ = std::fs::remove_file(path);
    r
}

#[test]
fn a_flipped_byte_in_each_region_is_named_and_never_a_wrong_answer() {
    let good = file_of(&[fact(1), fact(2), fact(3)]);
    let le64 = |at: usize| u64::from_le_bytes(good[at..at + 8].try_into().unwrap()) as usize;
    let (meta_at, index_at) = (le64(24), le64(36));

    let flip = |at: usize| {
        let mut b = good.clone();
        b[at] ^= 0x40;
        b
    };
    assert!(matches!(open_bytes("h", &flip(21)), Err(FdsError::BadChecksum("header"))));
    assert!(matches!(open_bytes("m", &flip(meta_at + 3)), Err(FdsError::BadChecksum("metadata"))));
    assert!(matches!(open_bytes("i", &flip(index_at + 1)), Err(FdsError::BadChecksum("index"))));

    // Inside a record: reading everything fails, and says which region...
    let record_at = meta_at + le64(32) % (1 << 32) + 5;
    let damaged = flip(record_at.min(index_at - 1));
    assert!(matches!(open_bytes("r", &damaged), Err(FdsError::BadChecksum("record"))));
    // ...while random access still serves every record that is intact.
    let path = scratch("random");
    std::fs::write(&path, &damaged).unwrap();
    let reader = DbReader::open(&path).unwrap();
    let results: Vec<bool> = (0..3).map(|i| reader.get(i).is_ok()).collect();
    assert_eq!(results.iter().filter(|ok| !**ok).count(), 1, "exactly one record is damaged: {results:?}");
    drop(reader);
    let _ = std::fs::remove_file(path);
}

#[test]
fn truncation_at_every_length_is_an_error_not_a_panic() {
    let good = file_of(&[fact(1), image_record()]);
    for cut in 0..good.len() {
        assert!(open_bytes("cut", &good[..cut]).is_err(), "a file cut at {cut} of {} bytes opened", good.len());
    }
}

#[test]
fn an_unknown_major_is_refused_and_a_foreign_file_is_not_ours() {
    let mut b = file_of(&[fact(1)]);
    b[8] = 5; // major 5
    assert!(matches!(open_bytes("major", &b), Err(FdsError::UnsupportedVersion(5))));
    assert!(matches!(open_bytes("magic", b"not an fds file at all, just some text.............................."), Err(FdsError::BadMagic)));
    assert!(matches!(open_bytes("png", &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0, 0, 0, 0, 0]), Err(FdsError::BadMagic | FdsError::Truncated)));
}

/// Builds a file by hand, the way a writer of another language would, so the
/// test depends on the spec and not on `encode_db`.
fn handmade(minor: u16, reserved: [u8; 8], records: Vec<Vec<u8>>) -> Vec<u8> {
    let meta = {
        let mut w = Writer::new();
        w.map(1);
        w.text("name");
        w.text("handmade");
        w.out
    };
    let mut index = Vec::new();
    let mut cursor = (HEADER_LEN + meta.len()) as u64;
    for r in &records {
        index.extend_from_slice(&cursor.to_le_bytes());
        index.extend_from_slice(&(r.len() as u32).to_le_bytes());
        index.extend_from_slice(&crc32(r).to_le_bytes());
        cursor += r.len() as u64;
    }
    let mut h = Vec::new();
    h.extend_from_slice(&[0x89, b'F', b'D', b'S', 0x0D, 0x0A, 0x1A, 0x0A]);
    h.extend_from_slice(&4u16.to_le_bytes());
    h.extend_from_slice(&minor.to_le_bytes());
    h.extend_from_slice(&7u32.to_le_bytes()); // application_id: someone else's
    h.extend_from_slice(&9u32.to_le_bytes()); // user_version
    h.extend_from_slice(&(records.len() as u32).to_le_bytes());
    h.extend_from_slice(&(HEADER_LEN as u64).to_le_bytes());
    h.extend_from_slice(&(meta.len() as u32).to_le_bytes());
    h.extend_from_slice(&cursor.to_le_bytes());
    h.extend_from_slice(&crc32(&meta).to_le_bytes());
    h.extend_from_slice(&crc32(&index).to_le_bytes());
    h.extend_from_slice(&reserved);
    let c = crc32(&h);
    h.extend_from_slice(&c.to_le_bytes());
    let mut out = h;
    out.extend_from_slice(&meta);
    for r in &records {
        out.extend_from_slice(r);
    }
    out.extend_from_slice(&index);
    out
}

/// A record the way a future 4.1 writer might write it: a key this reader has
/// never heard of, holding a float, a boolean and a nested map.
fn record_from_the_future() -> Vec<u8> {
    let mut w = Writer::new();
    w.map(11);
    for (k, v) in [("id", "f"), ("kind", "knowledge"), ("collection", "c"), ("body", "b"), ("title", "t"), ("source", "s"), ("verified_by", "human"), ("last_verified", "2030-01-01")] {
        w.text(k);
        w.text(v);
    }
    w.text("tokens");
    w.uint(3);
    w.text("sparkle"); // unknown key, nested unknown types
    w.map(2);
    w.text("shine");
    w.raw(&[0xf9, 0x3e, 0x00]); // half-precision 1.5
    w.text("on");
    w.raw(&[0xf5]); // true
    w.text("flavour"); // another unknown key
    w.array(2);
    w.uint(1);
    w.null();
    w.out
}

#[test]
fn a_newer_minor_is_read_ignoring_what_it_does_not_know_and_never_rewritten() {
    let path = scratch("future");
    let bytes = handmade(1, [0xAA; 8], vec![record_from_the_future()]);
    std::fs::write(&path, &bytes).unwrap();

    // Read: the unknown keys and the non-zero reserved bytes are ignored.
    let reader = DbReader::open(&path).unwrap();
    assert_eq!((reader.version(), reader.minor()), (4, 1));
    let (meta, recs) = read_db(&path).unwrap();
    drop(reader);
    assert_eq!(meta.name, "handmade");
    assert_eq!((recs[0].id.as_str(), recs[0].kind, recs[0].tokens), ("f", Kind::Knowledge, 3));

    // Rewrite: refused, because saving would drop `sparkle` and `flavour`.
    let err = write_db(&path, &meta, &recs).unwrap_err();
    assert!(matches!(err, FdsError::NewerFormat { major: 4, minor: 1 }), "{err:?}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "the file is untouched");
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_record_that_breaks_the_rules_is_refused() {
    let one = |build: &dyn Fn(&mut Writer)| {
        let mut w = Writer::new();
        build(&mut w);
        decode_record(&w.out)
    };
    // Missing required keys.
    assert!(one(&|w| {
        w.map(1);
        w.text("id");
        w.text("x");
    })
    .is_err());
    // A key twice.
    assert!(one(&|w| {
        w.map(2);
        w.text("id");
        w.text("x");
        w.text("id");
        w.text("y");
    })
    .is_err());
    // An unknown kind.
    let mut r = encode_record(&fact(1));
    let at = r.windows(4).position(|w| w == b"fact").unwrap();
    r[at..at + 4].copy_from_slice(b"fakt");
    assert!(decode_record(&r).is_err());
    // Bytes after the record.
    let mut r = encode_record(&fact(1));
    r.push(0);
    assert!(decode_record(&r).is_err());
}

#[test]
fn a_record_pointing_outside_the_records_region_is_refused() {
    let r = encode_record(&fact(1));
    let mut b = handmade(0, [0; 8], vec![r]);
    let index_at = u64::from_le_bytes(b[36..44].try_into().unwrap()) as usize;
    b[index_at..index_at + 8].copy_from_slice(&4u64.to_le_bytes()); // offset inside the header
    // Re-seal the index CRC and the header CRC so only the offset is wrong.
    let ic = crc32(&b[index_at..]);
    b[48..52].copy_from_slice(&ic.to_le_bytes());
    let hc = crc32(&b[..60]);
    b[60..64].copy_from_slice(&hc.to_le_bytes());
    let path = scratch("outside");
    std::fs::write(&path, &b).unwrap();
    let reader = DbReader::open(&path).unwrap();
    assert!(matches!(reader.get(0), Err(FdsError::Malformed(_))));
    drop(reader);
    let _ = std::fs::remove_file(path);
}

// ── older generations ────────────────────────────────────────────────────

#[test]
fn a_version_3_file_reads_as_version_4_and_migrates_on_save() {
    let path = scratch("from-v3");
    let meta = DbMeta { name: "soporte".into(), ..Default::default() };
    let recs = vec![image_record(), fact(7)];
    fds::v3::write_db(&path, &meta, &recs).unwrap();
    assert_eq!(format_version(&path).unwrap(), 3);

    let (m, got) = read_db(&path).unwrap();
    assert_eq!((m, got.clone()), (meta.clone(), recs.clone()));
    assert_eq!(DbReader::open(&path).unwrap().version(), 3);

    write_db(&path, &meta, &got).unwrap();
    assert_eq!(format_version(&path).unwrap(), 4);
    assert_eq!(read_db(&path).unwrap().1, recs, "nothing is lost on the way");
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_version_2_file_reads_as_version_4_and_migrates_on_save() {
    let path = scratch("from-v2");
    let old = fds::v2::Record {
        id: "0011223344556677".into(),
        kind: fds::v2::Kind::Procedure,
        collection: "default".into(),
        question: Some("flush the dns cache".into()),
        body: "{\"steps\":[{\"action\":\"run\",\"command\":\"ipconfig /flushdns\"}]}".into(),
        title: "Flush the DNS cache".into(),
        source: "winops/commands.json".into(),
        verified_by: "human".into(),
        last_verified: "2026-09-23".into(),
        tokens: 14,
        embedding: Some(vec![0.1; 4]),
    };
    let old_meta = fds::v2::DbMeta { name: "winops".into(), embedding_model: "all-minilm".into(), embedding_dims: 4, ..Default::default() };
    fds::v2::write_db(&path, &old_meta, std::slice::from_ref(&old)).unwrap();
    assert_eq!(format_version(&path).unwrap(), 2);

    let (meta, recs) = read_db(&path).unwrap();
    assert_eq!(meta.embedding_model, "all-minilm");
    let r = &recs[0];
    assert_eq!((r.id.as_str(), r.kind, r.question.as_deref()), ("0011223344556677", Kind::Procedure, Some("flush the dns cache")));
    assert_eq!(r.embedding.as_deref(), Some(&[0.1f32; 4][..]));
    assert!(r.rtype.is_empty() && r.attrs.is_empty() && r.links.is_empty());

    write_db(&path, &meta, &recs).unwrap();
    assert_eq!(format_version(&path).unwrap(), 4);
    assert_eq!(read_db(&path).unwrap().1, recs);
    let _ = std::fs::remove_file(path);
}
