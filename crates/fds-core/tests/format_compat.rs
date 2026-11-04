use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fds::{
    v2, v3,
    v4::{self, DbMeta, DbReader, Kind, Record},
    FdsError,
};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

struct TestFile(PathBuf);

impl TestFile {
    fn new(label: &str) -> Self {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::current_dir()
            .expect("current directory")
            .join("target")
            .join("fds-test-files");
        fs::create_dir_all(&directory).expect("create test fixture directory");
        Self(directory.join(format!("fds-{label}-{}-{id}.fds", std::process::id())))
    }

    fn write(&self, bytes: &[u8]) {
        fs::write(&self.0, bytes).expect("write test database");
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let mut lock = self.0.as_os_str().to_owned();
        lock.push(".lock");
        let _ = fs::remove_file(PathBuf::from(lock));
    }
}

fn meta() -> DbMeta {
    DbMeta {
        name: "compatibility-test".into(),
        created: "2026-01-02".into(),
        updated: "2026-01-03".into(),
        embedding_model: "test-embedder".into(),
        embedding_dims: 2,
    }
}

fn record() -> Record {
    Record {
        id: "fact-1".into(),
        kind: Kind::Fact,
        collection: "agent-guidance".into(),
        question: Some("What is the service port?".into()),
        body: "443".into(),
        title: "Service port".into(),
        source: "network-spec".into(),
        verified_by: "human".into(),
        last_verified: "2026-01-03".into(),
        tokens: 1,
        embedding: Some(vec![0.25, -0.5]),
        rtype: "configuration".into(),
        attrs: [("environment".into(), "test".into())].into(),
        links: vec!["service host".into()],
    }
}

fn write_legacy_v2(path: &Path) {
    let old_meta = v2::DbMeta {
        name: meta().name,
        created: meta().created,
        updated: meta().updated,
        embedding_model: meta().embedding_model,
        embedding_dims: meta().embedding_dims,
    };
    let old_record = v2::Record {
        id: "fact-1".into(),
        kind: v2::Kind::Fact,
        collection: "agent-guidance".into(),
        question: Some("What is the service port?".into()),
        body: "443".into(),
        title: "Service port".into(),
        source: "network-spec".into(),
        verified_by: "human".into(),
        last_verified: "2026-01-03".into(),
        tokens: 1,
        embedding: Some(vec![0.25, -0.5]),
    };
    v2::write_db(path, &old_meta, &[old_record]).expect("write v2 fixture");
}

#[test]
fn v4_round_trips_metadata_and_records() {
    let file = TestFile::new("round-trip");
    let expected_meta = meta();
    let expected_record = record();

    v4::write_db(&file.0, &expected_meta, &[expected_record.clone()]).unwrap();
    let (actual_meta, actual_records) = v4::read_db(&file.0).unwrap();

    assert_eq!(actual_meta, expected_meta);
    assert_eq!(actual_records, vec![expected_record]);
}

#[test]
fn v4_detects_corruption_in_each_checksummed_region() {
    let original = v4::encode_db(&meta(), &[record()]).unwrap();

    let mut header = original.clone();
    header[16] ^= 0x01;
    let file = TestFile::new("bad-header");
    file.write(&header);
    assert!(matches!(
        DbReader::open(&file.0),
        Err(FdsError::BadChecksum("header"))
    ));
    drop(file);

    let meta_offset = u64::from_le_bytes(original[24..32].try_into().unwrap()) as usize;
    let mut metadata = original.clone();
    metadata[meta_offset] ^= 0x01;
    let file = TestFile::new("bad-metadata");
    file.write(&metadata);
    assert!(matches!(
        DbReader::open(&file.0),
        Err(FdsError::BadChecksum("metadata"))
    ));
    drop(file);

    let index_offset = u64::from_le_bytes(original[36..44].try_into().unwrap()) as usize;
    let mut index = original.clone();
    index[index_offset] ^= 0x01;
    let file = TestFile::new("bad-index");
    file.write(&index);
    assert!(matches!(
        DbReader::open(&file.0),
        Err(FdsError::BadChecksum("index"))
    ));
    drop(file);

    let record_offset =
        u64::from_le_bytes(original[index_offset..index_offset + 8].try_into().unwrap()) as usize;
    let mut body = original;
    body[record_offset] ^= 0x01;
    let file = TestFile::new("bad-record");
    file.write(&body);
    let reader = DbReader::open(&file.0).unwrap();
    assert!(matches!(
        reader.get(0),
        Err(FdsError::BadChecksum("record"))
    ));
}

#[test]
fn v2_files_are_read_and_mapped_to_current_records() {
    let file = TestFile::new("read-v2");
    write_legacy_v2(&file.0);

    let reader = DbReader::open(&file.0).unwrap();
    assert_eq!(reader.version(), 2);
    assert_eq!(reader.meta().name, "compatibility-test");
    let actual = reader.get(0).unwrap();
    assert_eq!(actual.id, "fact-1");
    assert_eq!(actual.kind, Kind::Fact);
    assert_eq!(actual.body, "443");
    assert_eq!(actual.embedding, Some(vec![0.25, -0.5]));
    assert!(actual.rtype.is_empty());
    assert!(actual.attrs.is_empty());
    assert!(actual.links.is_empty());
}

#[test]
fn v3_files_are_read_without_losing_v3_fields() {
    let file = TestFile::new("read-v3");
    v3::write_db(&file.0, &meta(), &[record()]).unwrap();

    let reader = DbReader::open(&file.0).unwrap();
    assert_eq!(reader.version(), 3);
    assert_eq!(reader.meta(), &meta());
    assert_eq!(reader.get(0).unwrap(), record());
}
