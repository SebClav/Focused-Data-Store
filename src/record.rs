//! Building records with stable, content-derived IDs.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use fds::v4::{Kind, Record};

use crate::text::{canonical, fold};

/// Build a record with a stable content-derived ID and current metadata.
///
/// Facts and procedures use their normalized question/intent as the ID seed;
/// knowledge records use their source identifier.
#[allow(clippy::too_many_arguments)]
pub fn record(
    kind: Kind,
    collection: &str,
    question: Option<&str>,
    body: String,
    title: &str,
    source: &str,
    verified_by: &str,
    rtype: &str,
) -> Record {
    let seed = match kind {
        Kind::Fact => format!("fact:{}", canonical(question.unwrap_or(""))),
        Kind::Procedure => format!("procedure:{}", canonical(question.unwrap_or(""))),
        Kind::Knowledge => format!("knowledge:{source}#0"),
    };
    let tokens = body.split_whitespace().count() as u32;
    Record {
        id: format!("{:016x}", fnv(seed.as_bytes())),
        kind,
        collection: collection.into(),
        question: question.map(canonical),
        tokens,
        body,
        title: title.into(),
        source: source.into(),
        verified_by: verified_by.into(),
        last_verified: today(),
        embedding: None,
        rtype: rtype.into(),
        attrs: BTreeMap::new(),
        links: vec![],
    }
}

/// Validate and construct a non-empty fact record.
pub fn fact(
    question: &str,
    answer: &str,
    collection: &str,
    source: &str,
) -> Result<Record, String> {
    if question.trim().is_empty() || answer.trim().is_empty() {
        return Err("a fact needs both a question and an answer".into());
    }
    Ok(record(
        Kind::Fact,
        collection,
        Some(question),
        answer.trim().into(),
        question,
        source,
        "human",
        "",
    ))
}

/// Stable ID of the context record called `name`.
pub fn context_id(name: &str) -> String {
    format!("{:016x}", fnv(format!("context:{}", fold(name)).as_bytes()))
}

/// 64-bit FNV-1a hash.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

/// Today's UTC date as YYYY-MM-DD.
pub fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    // Civil-from-days conversion (proleptic Gregorian calendar).
    let z = days as i64 + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    format!("{:04}-{:02}-{:02}", y + if m <= 2 { 1 } else { 0 }, m, d)
}
