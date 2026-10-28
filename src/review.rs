//! Review metadata for records created or approved through the CLI.

use fds::v4::Record;

use crate::record::today;

const STATUS: &str = "fds.review.status";
const REVIEWER: &str = "fds.review.reviewer";
const NOTE: &str = "fds.review.note";

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
pub fn mark_pending(record: &mut Record) {
    record.verified_by = "unreviewed".into();
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
