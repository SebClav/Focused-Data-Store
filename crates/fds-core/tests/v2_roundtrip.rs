//! v2 format: all three record kinds, optional embeddings, metadata, and
//! rejection of files that are not v2.

use fds::v2::{read_db, write_db, DbMeta, DbReader, Kind, Record};
use std::fs;

fn rec(id: &str, kind: Kind, question: Option<&str>, body: &str, emb: Option<Vec<f32>>) -> Record {
    Record {
        id: id.to_string(),
        kind,
        collection: "test".to_string(),
        question: question.map(|q| q.to_string()),
        body: body.to_string(),
        title: format!("title {id}"),
        source: "manual".to_string(),
        verified_by: "human".to_string(),
        last_verified: "2026-09-17".to_string(),
        tokens: body.split_whitespace().count() as u32,
        embedding: emb,
    }
}

fn sample() -> (DbMeta, Vec<Record>) {
    let meta = DbMeta {
        name: "test".into(),
        created: "2026-09-17".into(),
        updated: "2026-09-17".into(),
        embedding_model: "all-minilm".into(),
        embedding_dims: 3,
    };
    let records = vec![
        rec("f1", Kind::Fact, Some("what is x"), "x is 1", Some(vec![0.1, 0.2, 0.3])),
        rec("p1", Kind::Procedure, Some("submit form a"), r##"{"steps":[{"action":"click","selector":"#go"}]}"##, None),
        rec("k1", Kind::Knowledge, None, "some knowledge text here", Some(vec![0.4, 0.5, 0.6])),
    ];
    (meta, records)
}

#[test]
fn round_trips_all_kinds_and_metadata() {
    let path = &std::env::temp_dir().join(format!("fds-{}-test_v2.fds", std::process::id()));
    let (meta, records) = sample();
    write_db(path, &meta, &records).unwrap();

    let (meta2, records2) = read_db(path).unwrap();
    assert_eq!(meta2, meta);
    assert_eq!(records2, records);
    fs::remove_file(path).ok();
}

#[test]
fn random_access_reads_one_record() {
    let path = &std::env::temp_dir().join(format!("fds-{}-test_v2_random.fds", std::process::id()));
    let (meta, records) = sample();
    write_db(path, &meta, &records).unwrap();

    let reader = DbReader::open(path).unwrap();
    assert_eq!(reader.len(), 3);
    assert_eq!(reader.get(2).unwrap(), records[2]);
    drop(reader);
    fs::remove_file(path).ok();
}

#[test]
fn rewrite_replaces_file_atomically() {
    let path = &std::env::temp_dir().join(format!("fds-{}-test_v2_rewrite.fds", std::process::id()));
    let (meta, mut records) = sample();
    write_db(path, &meta, &records).unwrap();
    records.pop();
    write_db(path, &meta, &records).unwrap();
    assert_eq!(read_db(path).unwrap().1.len(), 2);
    assert!(!path.with_extension("fds.tmp").exists(), "temp file left behind");
    fs::remove_file(path).ok();
}

#[test]
fn empty_database_is_valid() {
    let path = &std::env::temp_dir().join(format!("fds-{}-test_v2_empty.fds", std::process::id()));
    write_db(path, &DbMeta::default(), &[]).unwrap();
    let reader = DbReader::open(path).unwrap();
    assert!(reader.is_empty());
    drop(reader);
    fs::remove_file(path).ok();
}

#[test]
fn rejects_v1_and_garbage() {
    let path = &std::env::temp_dir().join(format!("fds-{}-test_v2_bad.fds", std::process::id()));
    fs::write(path, b"FDS1\x01\x00\x00\x00not a v2 file at all, padding padding").unwrap();
    assert!(matches!(DbReader::open(path), Err(fds::FdsError::BadMagic)));
    fs::remove_file(path).ok();
}
