//! The conformance suite (../conformance): the files a second implementation,
//! written in Python from the spec alone, also has to read the same way.
//!
//! Every valid file must read into exactly the records in its `.json`, and,
//! unless the case says otherwise, writing those records back must reproduce
//! the file byte for byte. Every damaged file must fail with the category its
//! `.json` names, and a damaged record must not stop the intact ones from
//! being read.

use std::path::PathBuf;

use fds::v4::{decode_record, encode_db, read_db, DbMeta, DbReader, Record};
use fds::FdsError;
use serde_json::Value;

fn cases() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("conformance").join("cases")
}

fn manifest() -> Value {
    let path = cases().join("MANIFEST.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

fn names(group: &str) -> Vec<String> {
    manifest()[group].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

fn expected(group: &str, name: &str) -> Value {
    let path = cases().join(group).join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn category(e: &FdsError) -> &'static str {
    match e {
        FdsError::BadMagic => "not_fds",
        FdsError::UnsupportedVersion(_) => "unsupported_major",
        FdsError::Truncated => "truncated",
        FdsError::BadChecksum(_) => "checksum",
        FdsError::Malformed(_) => "malformed",
        other => panic!("not a category of the spec: {other:?}"),
    }
}

#[test]
fn every_valid_file_reads_as_expected_and_rewrites_identically() {
    let names = names("valid");
    assert!(names.len() >= 8, "the manifest lists {} valid files", names.len());
    for name in names {
        let path = cases().join("valid").join(format!("{name}.fds"));
        let exp = expected("valid", &name);

        let reader = DbReader::open(&path).unwrap_or_else(|e| panic!("valid/{name}: refused: {e:?}"));
        assert_eq!(reader.version(), 4, "valid/{name}");
        assert_eq!(reader.minor() as u64, exp["minor"].as_u64().unwrap(), "valid/{name}: minor");
        drop(reader);

        let (meta, records) = read_db(&path).unwrap_or_else(|e| panic!("valid/{name}: {e:?}"));
        let want_meta: DbMeta = serde_json::from_value(exp["meta"].clone()).unwrap();
        let want_records: Vec<Record> = serde_json::from_value(exp["records"].clone()).unwrap();
        assert_eq!(meta, want_meta, "valid/{name}: metadata");
        assert_eq!(records, want_records, "valid/{name}: records");

        if exp["reproducible"].as_bool().unwrap() {
            let again = encode_db(&meta, &records).unwrap();
            let original = std::fs::read(&path).unwrap();
            assert!(again == original, "valid/{name}: writing the records back does not reproduce the file");
        }
        if !exp["writable"].as_bool().unwrap() {
            let copy = std::env::temp_dir().join(format!("fds-conf-{name}-{}.fds", std::process::id()));
            std::fs::copy(&path, &copy).unwrap();
            let err = fds::v4::write_db(&copy, &meta, &records).unwrap_err();
            assert!(matches!(err, FdsError::NewerFormat { .. }), "valid/{name}: a rewrite must be refused, got {err:?}");
            assert_eq!(std::fs::read(&copy).unwrap(), std::fs::read(&path).unwrap(), "valid/{name}: the file was touched");
            let _ = std::fs::remove_file(copy);
        }
    }
}

#[test]
fn every_damaged_file_fails_the_way_its_case_says() {
    let names = names("invalid");
    assert!(names.len() >= 40, "the manifest lists {} damaged files", names.len());
    for name in names {
        let path = cases().join("invalid").join(format!("{name}.fds"));
        let exp = expected("invalid", &name);
        let want = exp["error"].as_str().unwrap();
        let stage = exp["stage"].as_str().unwrap();

        match DbReader::open(&path) {
            Err(e) => {
                assert_eq!(stage, "open", "invalid/{name}: failed at open, the case says {stage}: {e:?}");
                assert_eq!(category(&e), want, "invalid/{name}: {e:?}");
            }
            Ok(reader) => {
                assert_eq!(stage, "record", "invalid/{name}: opened, but it must not");
                let mut readable = Vec::new();
                let mut first_error = None;
                for i in 0..reader.len() {
                    match reader.get(i) {
                        Ok(_) => readable.push(i as u64),
                        Err(e) => {
                            first_error.get_or_insert(category(&e));
                        }
                    }
                }
                let want_readable: Vec<u64> = exp["readable"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
                assert_eq!(first_error, Some(want), "invalid/{name}");
                assert_eq!(readable, want_readable, "invalid/{name}: which records stay readable");
                drop(reader);
                let all = read_db(&path).unwrap_err();
                assert_eq!(category(&all), want, "invalid/{name}: reading everything");
            }
        }
    }
}

#[test]
fn a_good_file_cut_at_every_length_is_truncated() {
    let path = cases().join("valid").join("all_fields.fds");
    let data = std::fs::read(&path).unwrap();
    let cut_path = std::env::temp_dir().join(format!("fds-conf-cut-{}.fds", std::process::id()));
    for cut in 0..data.len() {
        std::fs::write(&cut_path, &data[..cut]).unwrap();
        let err = match read_db(&cut_path) {
            Ok(_) => panic!("a file cut at {cut} bytes opened"),
            Err(e) => e,
        };
        assert_eq!(category(&err), "truncated", "cut at {cut}: {err:?}");
    }
    let _ = std::fs::remove_file(cut_path);
}

#[test]
fn a_record_can_be_decoded_without_its_file() {
    // The record bytes of the first valid case are all a library needs to be
    // given: decode_record does not look at the container.
    let data = std::fs::read(cases().join("valid").join("one_fact.fds")).unwrap();
    let meta_len = u32::from_le_bytes(data[32..36].try_into().unwrap()) as usize;
    let index_at = u64::from_le_bytes(data[36..44].try_into().unwrap()) as usize;
    let record = decode_record(&data[64 + meta_len..index_at]).unwrap();
    assert_eq!(record.id, "0000000000000001");
}
