//! `fds check` and the policy helpers it shares with `fds policy`.

use std::path::Path;

use fds::v4::Record;

use crate::{db, policy, print_json, sectors};

pub fn policy_bounds(r: &Record) -> String {
    let a = |k: &str| r.attrs.get(k).cloned().unwrap_or_default();
    match a("rule").as_str() {
        "range" => format!(
            " [{}..{}{}{}]",
            a("min"),
            a("max"),
            if a("unit").is_empty() { "" } else { " " },
            a("unit")
        ),
        "one_of" => format!(" [{}]", a("values")),
        _ => String::new(),
    }
}

/// The policies that count, and what to answer when none matches: from one
/// database, or from every sector of a manifest (a policy counts wherever a
/// person verified it, and never otherwise).
pub fn policy_source(
    db_file: Option<&Path>,
    manifest: Option<&Path>,
) -> Result<(Vec<policy::Policy>, policy::Fallback), String> {
    if let Some(mpath) = manifest {
        let m = sectors::Manifest::load(mpath)?;
        let mut all = Vec::new();
        for def in &m.sectors {
            let (_, records) = db::load(&m.path_of(def))?;
            all.extend(policy::policies(&records));
        }
        let fallback = m
            .default
            .as_deref()
            .map(policy::Fallback::parse)
            .transpose()?
            .unwrap_or_default();
        return Ok((all, fallback));
    }
    let path = db::locate(db_file)?;
    let (_, records) = db::load(&path)?;
    Ok((policy::policies(&records), policy::Fallback::default()))
}

pub fn check_cmd(
    subject: &str,
    action: &str,
    value: Option<&str>,
    manifest: Option<&Path>,
    default: Option<&str>,
    db_file: Option<&Path>,
    json_out: bool,
) -> Result<u8, String> {
    let (policies, mut fallback) = policy_source(db_file, manifest)?;
    if let Some(d) = default {
        fallback = policy::Fallback::parse(d)?;
    }
    let decision = policy::check(&policies, subject, action, value, fallback);
    if json_out {
        print_json(&serde_json::to_value(&decision).map_err(|e| e.to_string())?);
    } else {
        println!("{}", decision.summary());
    }
    Ok(if decision.allowed() {
        0
    } else {
        policy::DENY_EXIT
    })
}
