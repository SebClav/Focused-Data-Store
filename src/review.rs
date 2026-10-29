//! Review metadata for records created or approved through the CLI.

use fds::v4::Record;

use crate::record::today;

const STATUS: &str = "fds.review.status";
const REVIEWER: &str = "fds.review.reviewer";
const NOTE: &str = "fds.review.note";
/// The provenance label a record had before its approval replaced it.
const ORIGIN: &str = "fds.origin";

/// Existing labels alone do not establish that an explicit review occurred.
pub fn status(record: &Record) -> &'static str {
    let approved = record.attrs.get(STATUS).is_some_and(|s| s == "approved")
        && record
            .attrs
            .get(REVIEWER)
            .is_some_and(|s| !s.trim().is_empty())
        && record.verified_by == "human"
        && !record.last_verified.is_empty();
    if approved {
        "approved"
    } else {
        "pending"
    }
}

/// A new or replaced record must be reviewed again.
///
/// `human` is a claim only an approval may make, so it is withdrawn. Any
/// other label (`ingest`, `model:<name>`, ...) says where the record came
/// from, not that someone checked it, and is kept.
pub fn mark_pending(record: &mut Record) {
    if record.verified_by == "human" {
        record.verified_by = "unreviewed".into();
    }
    record.last_verified.clear();
    record.attrs.insert(STATUS.into(), "pending".into());
    record.attrs.remove(REVIEWER);
    record.attrs.remove(NOTE);
}

/// Record a self-declared review. Authentication is the host's responsibility.
pub fn approve(record: &mut Record, reviewer: &str, note: Option<&str>) -> Result<(), String> {
    let reviewer = reviewer.trim();
    if reviewer.is_empty() {
        return Err("reviewer must not be empty".into());
    }
    // Approval replaces the provenance label; keep it so it is not lost.
    if !matches!(record.verified_by.as_str(), "human" | "unreviewed") {
        let origin = record.verified_by.clone();
        record.attrs.entry(ORIGIN.into()).or_insert(origin);
    }
    record.verified_by = "human".into();
    record.last_verified = today();
    record.attrs.insert(STATUS.into(), "approved".into());
    record.attrs.insert(REVIEWER.into(), reviewer.into());
    match note.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => {
            record.attrs.insert(NOTE.into(), text.into());
        }
        None => {
            record.attrs.remove(NOTE);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fds::v4::{Kind, Record};

    use super::{approve, mark_pending, status};

    fn record(verified_by: &str) -> Record {
        Record {
            id: "id".into(),
            kind: Kind::Knowledge,
            collection: "kb".into(),
            question: None,
            body: "text".into(),
            title: "doc".into(),
            source: "doc.txt".into(),
            verified_by: verified_by.into(),
            last_verified: "2026-01-01".into(),
            tokens: 1,
            embedding: None,
            rtype: String::new(),
            attrs: BTreeMap::new(),
            links: Vec::new(),
        }
    }

    #[test]
    fn a_claim_of_human_is_withdrawn_when_a_record_becomes_pending() {
        let mut r = record("human");
        mark_pending(&mut r);
        assert_eq!(r.verified_by, "unreviewed");
        assert_eq!(status(&r), "pending");
        assert!(r.last_verified.is_empty());
    }

    #[test]
    fn provenance_labels_survive_becoming_pending() {
        for label in ["ingest", "model:some-model"] {
            let mut r = record(label);
            mark_pending(&mut r);
            assert_eq!(r.verified_by, label);
            assert_eq!(status(&r), "pending");
        }
    }

    #[test]
    fn approval_keeps_the_provenance_it_replaces() {
        let mut r = record("ingest");
        mark_pending(&mut r);
        approve(&mut r, "Ada", None).unwrap();
        assert_eq!(status(&r), "approved");
        assert_eq!(r.verified_by, "human");
        assert_eq!(r.attrs["fds.origin"], "ingest");
    }

    #[test]
    fn approving_twice_does_not_overwrite_the_origin() {
        let mut r = record("model:some-model");
        mark_pending(&mut r);
        approve(&mut r, "Ada", None).unwrap();
        approve(&mut r, "Grace", Some("again")).unwrap();
        assert_eq!(r.attrs["fds.origin"], "model:some-model");
        assert_eq!(r.attrs["fds.review.reviewer"], "Grace");
    }

    #[test]
    fn a_proposal_with_no_provenance_gets_no_origin() {
        let mut r = record("unreviewed");
        approve(&mut r, "Ada", None).unwrap();
        assert!(!r.attrs.contains_key("fds.origin"));
    }
}
