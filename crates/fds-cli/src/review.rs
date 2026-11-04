//! `fds review`: list the lines agents flagged in the journal and open the editor on them.
//!
//! An agent flags a line by naming it: `--file backend/server.js:42-58`, or `path:line` written in the
//! summary or the reason. Editor integrations are separate projects; they read the same flags from
//! `fds --json journal show`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::json;

use crate::cli::ReviewArgs;
use crate::cmd_journal::journal_entries;
use crate::journal::Entry;

const EXTENSIONS: &str = "js|mjs|cjs|ts|tsx|jsx|py|rs|go|java|html|css|json|md|sql|sh|toml|yaml|yml|c|h|cpp|cs|php|rb";

/// A file an entry names, with the lines it points at when it gave them.
#[derive(Debug, Clone, PartialEq)]
pub struct Flag {
    pub entry: String,
    pub agent: String,
    pub summary: String,
    pub why: String,
    pub reviewed: bool,
    pub sector: String,
    pub path: String,
    pub start: Option<u32>,
    pub end: Option<u32>,
}

fn ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(.+?)(?::|#L?)(\d+)(?:-L?(\d+))?$").unwrap())
}

fn text_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(r"([A-Za-z0-9_./\\-]+\.(?:{EXTENSIONS})):(\d+)(?:-(\d+))?")).unwrap()
    })
}

fn norm(p: &str) -> String {
    let s = p.trim().replace('\\', "/");
    s.strip_prefix("./").unwrap_or(&s).to_string()
}

/// `src/a.rs`, `src/a.rs:12`, `src/a.rs:12-20` or `src/a.rs#L12-L20`.
pub fn parse_ref(s: &str) -> (String, Option<u32>, Option<u32>) {
    match ref_re().captures(s.trim()) {
        Some(c) => {
            let start: u32 = c[2].parse().unwrap_or(0);
            let end = c.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(start);
            (norm(&c[1]), Some(start), Some(end))
        }
        None => (norm(s), None, None),
    }
}

/// Every file the entry names: in `files`, and as `path:line` inside its summary or reason.
pub fn flags_of(e: &Entry) -> Vec<Flag> {
    let mut out: Vec<Flag> = Vec::new();
    let mut seen: HashSet<(String, Option<u32>, Option<u32>)> = HashSet::new();
    let mut add = |path: String, start: Option<u32>, end: Option<u32>| {
        if path.is_empty() || !seen.insert((path.clone(), start, end)) {
            return;
        }
        out.push(Flag {
            entry: e.id.clone(),
            agent: e.agent.clone(),
            summary: e.summary.clone(),
            why: e.why.clone(),
            reviewed: e.reviewed,
            sector: e.sector.clone(),
            path,
            start,
            end,
        });
    };
    for f in &e.files {
        let (p, s, en) = parse_ref(f);
        add(p, s, en);
    }
    for text in [&e.why, &e.summary] {
        for c in text_ref_re().captures_iter(text) {
            let start: u32 = c[2].parse().unwrap_or(0);
            let end = c.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(start);
            add(norm(&c[1]), Some(start), Some(end));
        }
    }
    out
}

fn span(f: &Flag) -> String {
    match (f.start, f.end) {
        (Some(a), Some(b)) if a != b => format!("{}:{a}-{b}", f.path),
        (Some(a), _) => format!("{}:{a}", f.path),
        _ => f.path.clone(),
    }
}

/// Windows canonical paths start with `\\?\`, which editors do not understand in `--goto`.
fn plain(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

fn launch(editor: &str, args: &[String], envs: &[(&str, String)]) -> Result<(), String> {
    let mut cmd = if cfg!(windows) {
        // `code` is a .cmd shim on Windows
        let mut c = Process::new("cmd");
        c.arg("/C").arg(editor);
        c
    } else {
        Process::new(editor)
    };
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("cannot start `{editor}`: {e}"))
}

pub fn review_cmd(args: &ReviewArgs, db_file: Option<&Path>, json_out: bool) -> Result<u8, String> {
    let editor = args
        .editor
        .clone()
        .or_else(|| std::env::var("FDS_EDITOR").ok())
        .unwrap_or_else(|| "code".to_string());
    let (entries, _) = journal_entries(&args.src, db_file)?;
    let mut flags: Vec<Flag> = Vec::new();
    for e in &entries {
        if !args.all && e.reviewed {
            continue;
        }
        if let Some(id) = &args.entry {
            if !e.id.starts_with(id.as_str()) {
                continue;
            }
        }
        flags.extend(flags_of(e));
    }
    flags.sort_by(|a, b| (&a.path, a.start).cmp(&(&b.path, b.start)));

    if json_out {
        let v: Vec<_> = flags
            .iter()
            .map(|f| json!({"entry": f.entry, "agent": f.agent, "summary": f.summary, "why": f.why, "reviewed": f.reviewed,
                "sector": f.sector, "path": f.path, "start": f.start, "end": f.end}))
            .collect();
        crate::print_json(&serde_json::Value::Array(v));
        return Ok(0);
    }
    if flags.is_empty() {
        println!("nothing flagged{}", if args.all { "" } else { " for review (--all includes reviewed entries)" });
        return Ok(0);
    }
    for f in &flags {
        println!("{:<34} {:<10} {}", span(f), if f.agent.is_empty() { "person" } else { &f.agent }, f.summary);
    }
    if args.list {
        return Ok(0);
    }

    let root = args.root.clone().unwrap_or_else(|| PathBuf::from("."));
    let root = plain(std::fs::canonicalize(&root).map_err(|e| format!("{}: {e}", root.display()))?);
    let target = flags
        .iter()
        .find(|f| f.start.is_some() && root.join(&f.path).is_file())
        .or_else(|| flags.iter().find(|f| root.join(&f.path).is_file()));
    let Some(target) = target else {
        return Err(format!("none of the flagged files exists under {} (use --root)", root.display()));
    };
    let file = root.join(&target.path);
    let goto = format!("{}:{}:1", file.display(), target.start.unwrap_or(1));
    let mut envs: Vec<(&str, String)> = Vec::new();
    if let Some(db) = db_file {
        envs.push(("FDS_DB", db.display().to_string()));
    }
    if let Some(s) = &args.src.sectors {
        envs.push(("FDS_SECTORS", s.display().to_string()));
    }
    if let Ok(exe) = std::env::current_exe() {
        envs.push(("FDS_BIN", plain(exe).display().to_string()));
    }
    launch(&editor, &["--reuse-window".into(), root.display().to_string(), "--goto".into(), goto], &envs)?;
    println!("opened {} in {editor}", span(target));
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(files: &[&str], why: &str) -> Entry {
        Entry {
            id: "abc123".into(),
            summary: "Order server".into(),
            why: why.into(),
            files: files.iter().map(|s| s.to_string()).collect(),
            branch: String::new(),
            commit: String::new(),
            agent: "backend".into(),
            at: String::new(),
            reviewed: false,
            sector: String::new(),
            role: String::new(),
            status: String::new(),
            to: String::new(),
            seq: 1,
            protected: false,
        }
    }

    #[test]
    fn a_reference_has_a_path_and_maybe_lines() {
        assert_eq!(parse_ref("backend/server.js"), ("backend/server.js".into(), None, None));
        assert_eq!(parse_ref("backend/server.js:12"), ("backend/server.js".into(), Some(12), Some(12)));
        assert_eq!(parse_ref("backend/server.js:12-20"), ("backend/server.js".into(), Some(12), Some(20)));
        assert_eq!(parse_ref("src\\a.rs#L5-L9"), ("src/a.rs".into(), Some(5), Some(9)));
        assert_eq!(parse_ref("C:/work/a.js:3"), ("C:/work/a.js".into(), Some(3), Some(3)));
    }

    #[test]
    fn flags_come_from_files_and_from_the_text() {
        let e = entry(&["a.js:3-5", "b.css"], "Unsure about app.js:40 and a.js:3-5");
        let f = flags_of(&e);
        let got: Vec<_> = f.iter().map(|x| (x.path.as_str(), x.start, x.end)).collect();
        assert_eq!(got, vec![("a.js", Some(3), Some(5)), ("b.css", None, None), ("app.js", Some(40), Some(40))]);
    }

    #[test]
    fn the_windows_verbatim_prefix_is_dropped() {
        assert_eq!(plain(PathBuf::from(r"\\?\C:\work\a")), PathBuf::from(r"C:\work\a"));
        assert_eq!(plain(PathBuf::from("/work/a")), PathBuf::from("/work/a"));
    }
}
