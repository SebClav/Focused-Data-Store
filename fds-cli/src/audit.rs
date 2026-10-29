//! The audit file (SPEC.md §11.3): every decision the guard takes, one JSON
//! object per line, appended and never rewritten.
//!
//! Each line carries the SHA-256 of the line before it, so a line that is
//! changed, removed or inserted in the middle breaks the chain and `fds audit
//! verify` says where. A chain cannot show that the last lines were cut off:
//! keep the hash `verify` prints somewhere the agent cannot write, and compare.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::{db, journal, media};

pub const FILE: &str = "audit.jsonl";
const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// A line is read back from the end of the file; no line is longer than this.
const TAIL: u64 = 256 * 1024;
const MAX_TEXT: usize = 400;

/// `audit.jsonl` next to a manifest.
pub fn beside(manifest: &Path) -> PathBuf {
    manifest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).join(FILE)
}

/// Long strings in arguments are cut: the audit shows what was asked, it must
/// not become a second copy of everything an agent handled.
pub fn shorten(v: &Value) -> Value {
    match v {
        Value::String(s) if s.chars().count() > MAX_TEXT => Value::String(format!("{}\u{2026} [{} chars]", s.chars().take(MAX_TEXT).collect::<String>(), s.chars().count())),
        Value::Array(a) => Value::Array(a.iter().map(shorten).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shorten(v))).collect()),
        other => other.clone(),
    }
}

fn last_line(path: &Path) -> Result<Option<String>, String> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(TAIL))).map_err(|e| e.to_string())?;
    // The tail may begin in the middle of a character: read bytes, not text.
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let tail = String::from_utf8_lossy(&buf);
    Ok(tail.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
}

/// Appends one entry under the file's lock, with its number and the hash of
/// the line before it. `fields` are the entry's own: `at`, `agent`, `tool`...
pub fn append(path: &Path, mut fields: Map<String, Value>) -> Result<(), String> {
    let _lock = db::lock(path)?;
    let previous = last_line(path)?;
    let (n, prev) = match &previous {
        Some(line) => {
            let n = serde_json::from_str::<Value>(line).ok().and_then(|v| v.get("n").and_then(|n| n.as_u64())).unwrap_or(0);
            (n + 1, media::sha256_hex(line.as_bytes()))
        }
        None => (1, ZERO.to_string()),
    };
    fields.insert("n".into(), json!(n));
    fields.insert("prev".into(), json!(prev));
    let line = serde_json::to_string(&Value::Object(fields)).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())?;
    f.sync_data().map_err(|e| e.to_string())
}

pub struct Report {
    pub lines: usize,
    /// `None` when the whole chain holds.
    pub broken: Option<(usize, String)>,
    /// The hash of the last line: store it where the agent cannot write.
    pub head: String,
}

pub fn verify(path: &Path) -> Result<Report, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut prev = ZERO.to_string();
    let mut n = 0usize;
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return Ok(Report { lines: n, broken: Some((i + 1, "this line is not valid JSON".into())), head: prev }),
        };
        n += 1;
        if v.get("n").and_then(|x| x.as_u64()) != Some(n as u64) {
            return Ok(Report { lines: n, broken: Some((i + 1, format!("expected entry number {n}: a line was removed, inserted or reordered"))), head: prev });
        }
        if v.get("prev").and_then(|x| x.as_str()) != Some(prev.as_str()) {
            return Ok(Report { lines: n, broken: Some((i + 1, "the line before this one was changed".into())), head: prev });
        }
        prev = media::sha256_hex(line.as_bytes());
    }
    Ok(Report { lines: n, broken: None, head: prev })
}

/// The audit as timeline steps (see journal.rs), oldest first by number.
pub fn steps(path: &Path) -> Result<Vec<journal::Step>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let s = |v: &Value, k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|v| {
            let mut what = format!("{} {}", s(&v, "decision"), s(&v, "tool"));
            if let Some(c) = v.get("checks").and_then(|c| c.as_array()).and_then(|c| c.first()) {
                let value = c.get("value").and_then(|x| x.as_str()).map(|x| format!(" = {x}")).unwrap_or_default();
                what.push_str(&format!(" [{} {}{value}]", s(c, "action"), s(c, "subject")));
            }
            if !s(&v, "agent").is_empty() {
                what.push_str(&format!(" by {}", s(&v, "agent")));
            }
            journal::Step { at: s(&v, "at"), order: v.get("n").and_then(|x| x.as_u64()).unwrap_or(0), actor: "guard".into(), text: what, from: "audit".into() }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tool: &str) -> Map<String, Value> {
        json!({"at": "2026-10-23T10:00:00Z", "agent": "a", "tool": tool, "decision": "allow"}).as_object().unwrap().clone()
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fds-audit-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(FILE)
    }

    #[test]
    fn a_chain_of_entries_verifies_and_numbers_itself() {
        let p = scratch("chain");
        for t in ["one", "two", "three"] {
            append(&p, entry(t)).unwrap();
        }
        let r = verify(&p).unwrap();
        assert_eq!((r.lines, r.broken.is_none()), (3, true));
        assert_eq!(r.head.len(), 64);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.lines().next().unwrap().contains(&format!("\"prev\":\"{ZERO}\"")));
        let s = steps(&p).unwrap();
        assert_eq!((s.len(), s[2].order, s[0].actor.as_str()), (3, 3, "guard"));
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn changing_removing_or_inserting_a_line_is_found_and_located() {
        let p = scratch("tamper");
        for t in ["a", "b", "c", "d"] {
            append(&p, entry(t)).unwrap();
        }
        let lines: Vec<String> = std::fs::read_to_string(&p).unwrap().lines().map(String::from).collect();

        let write = |ls: &[String]| std::fs::write(&p, ls.join("\n") + "\n").unwrap();
        // 1. a line edited: the next one no longer matches it
        let mut edited = lines.clone();
        edited[1] = edited[1].replace("\"allow\"", "\"deny\"");
        write(&edited);
        let r = verify(&p).unwrap();
        assert_eq!(r.broken.as_ref().map(|b| b.0), Some(3), "the edit shows at the line after it: {:?}", r.broken);
        // 2. a line removed
        let removed = vec![lines[0].clone(), lines[2].clone(), lines[3].clone()];
        write(&removed);
        assert_eq!(verify(&p).unwrap().broken.map(|b| b.0), Some(2));
        // 3. a line inserted
        let mut inserted = lines.clone();
        inserted.insert(2, lines[1].clone());
        write(&inserted);
        assert!(verify(&p).unwrap().broken.is_some());
        // 4. untouched again
        write(&lines);
        assert!(verify(&p).unwrap().broken.is_none());
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// conformance/cases/audit: the chains every implementation must judge the same way.
    #[test]
    fn the_audit_conformance_cases_are_judged_as_expected() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("conformance").join("cases").join("audit");
        let exp: Value = serde_json::from_str(&std::fs::read_to_string(base.join("expected.json")).unwrap()).unwrap();
        for c in exp["cases"].as_array().unwrap() {
            let file = c["file"].as_str().unwrap();
            let r = verify(&base.join(file)).unwrap();
            let want = c["broken_at"].as_u64();
            assert_eq!(r.broken.as_ref().map(|b| b.0 as u64), want, "{file}: {:?}", r.broken);
            if c["intact"].as_bool().unwrap() {
                assert_eq!(r.head, exp["head_of_good"].as_str().unwrap(), "the head hash of {file}");
            }
        }
    }

    #[test]
    fn long_arguments_are_cut() {
        let long = "x".repeat(5000);
        let v = shorten(&json!({"a": [long.clone()], "b": "short"}));
        assert!(v["a"][0].as_str().unwrap().len() < 600 && v["a"][0].as_str().unwrap().contains("[5000 chars]"));
        assert_eq!(v["b"], "short");
    }
}
