//! The change journal (SPEC.md §3.5): what an agent did, written for the
//! developer who reads the code afterwards and was not there.
//!
//! An entry is an ordinary record of `type: change`, so it needs no new kind,
//! no new file format and no new storage. This module builds those records,
//! reads them back, renders them as Markdown, and can publish the Markdown to
//! a git branch (`fds` by default) without touching the working tree, the index
//! or the branch that is checked out.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use fds::v4::{Kind, Record};
use serde::Serialize;

use crate::db;
use crate::text::{canonical, stable_id, word_count};

/// The `type` of a journal record.
pub const TYPE: &str = "change";
pub const COLLECTION: &str = "journal";
pub const DEFAULT_BRANCH: &str = "fds";

/// What the author of an entry says.
#[derive(Default, Clone)]
pub struct Draft<'a> {
    pub summary: &'a str,
    pub why: &'a str,
    pub files: &'a [String],
    pub branch: &'a str,
    pub commit: &'a str,
    /// The agent's name; empty when a person wrote it.
    pub agent: &'a str,
    /// `2026-10-21T14:02:11Z`.
    pub at: &'a str,
    /// The part this step plays, in the owner's words: `me`, `reasoning`,
    /// `decision`, `execution`... Free text, optional.
    pub role: &'a str,
    /// Where the step stands: `running`, `finished`, `blocked`... Free text, optional.
    pub status: &'a str,
    /// Who the step was handed to, when it was a delegation.
    pub to: &'a str,
}

/// An entry as a reader sees it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub summary: String,
    pub why: String,
    pub files: Vec<String>,
    pub branch: String,
    pub commit: String,
    /// Empty when a person wrote it.
    pub agent: String,
    pub at: String,
    /// A person has checked it (`verified_by` is `human`).
    pub reviewed: bool,
    /// Which sector it came from, when read across several.
    pub sector: String,
    pub role: String,
    pub status: String,
    pub to: String,
    /// Position in its database, counted in the order the entries were written.
    pub seq: u64,
    /// The record reached this reader as a stand-in (§11.4): only the title and
    /// the owner's `derived` text are left. Not part of the published JSON.
    #[serde(skip)]
    pub protected: bool,
}

/// Builds the record for an entry. `verified_by` is `model:<name>` for an
/// agent, which waits for a person to review it, or `human`.
pub fn record(d: &Draft, verified_by: &str) -> Result<Record, String> {
    let summary = d.summary.split_whitespace().collect::<Vec<_>>().join(" ");
    if canonical(&summary).is_empty() {
        return Err("a journal entry needs a summary: one line saying what changed".into());
    }
    if summary.chars().count() > 200 {
        return Err(
            "the summary is one line, 200 characters at most; put the detail in `why`".into(),
        );
    }
    let why = d.why.trim();
    let body = if why.is_empty() {
        summary.clone()
    } else {
        why.to_string()
    };

    let mut files: Vec<String> = Vec::new();
    for f in d.files.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
        if !files.iter().any(|x| x == f) {
            files.push(f.to_string());
        }
    }

    let mut attrs = std::collections::BTreeMap::new();
    attrs.insert("at".to_string(), d.at.to_string());
    for (k, v) in [
        ("agent", d.agent),
        ("branch", d.branch),
        ("commit", d.commit),
        ("role", d.role),
        ("status", d.status),
        ("to", d.to),
    ] {
        if !v.trim().is_empty() {
            attrs.insert(k.to_string(), v.trim().to_string());
        }
    }
    if !files.is_empty() {
        attrs.insert("files".to_string(), files.join("\n"));
    }

    let source = if d.agent.trim().is_empty() {
        "cli".to_string()
    } else {
        format!("agent:{}", d.agent.trim())
    };
    Ok(Record {
        id: stable_id(&format!(
            "change:{}:{}:{}",
            d.agent.trim(),
            d.at,
            canonical(&summary)
        )),
        kind: Kind::Knowledge,
        collection: COLLECTION.to_string(),
        question: None,
        title: summary,
        tokens: word_count(&body),
        body,
        source,
        verified_by: verified_by.to_string(),
        last_verified: d.at.get(..10).unwrap_or(d.at).to_string(),
        embedding: None,
        rtype: TYPE.to_string(),
        attrs,
        links: Vec::new(),
    })
}

/// Writes an entry into a database: takes the lock, re-reads, adds, saves.
/// Returns the record's id.
pub fn append(path: &Path, mut rec: Record) -> Result<String, String> {
    let _lock = db::lock(path)?;
    let (mut meta, mut records) = db::load(path)?;
    let id = rec.id.clone();
    // Entries keep the order they were written in, even within one second.
    let next = records
        .iter()
        .filter(|r| r.rtype == TYPE && r.id != id)
        .filter_map(|r| r.attrs.get("seq").and_then(|s| s.parse::<u64>().ok()))
        .max()
        .unwrap_or(0)
        + 1;
    let seq = records
        .iter()
        .find(|r| r.id == id)
        .and_then(|r| r.attrs.get("seq").cloned())
        .unwrap_or_else(|| next.to_string());
    rec.attrs.insert("seq".to_string(), seq);
    match records.iter().position(|r| r.id == id) {
        Some(i) => records[i] = rec,
        None => records.push(rec),
    }
    db::save(path, &mut meta, &records)?;
    Ok(id)
}

pub fn entry_of(r: &Record, sector: &str) -> Option<Entry> {
    if r.rtype != TYPE {
        return None;
    }
    let attr = |k: &str| r.attrs.get(k).cloned().unwrap_or_default();
    let summary = r.title.clone();
    Some(Entry {
        id: r.id.clone(),
        why: if r.body == summary {
            String::new()
        } else {
            r.body.clone()
        },
        summary,
        files: attr("files")
            .lines()
            .map(str::to_string)
            .filter(|f| !f.is_empty())
            .collect(),
        branch: attr("branch"),
        commit: attr("commit"),
        agent: attr("agent"),
        // A record written by hand may lack `at`: fall back to the day it was verified.
        at: r
            .attrs
            .get("at")
            .cloned()
            .unwrap_or_else(|| r.last_verified.clone()),
        reviewed: r.verified_by == "human",
        sector: sector.to_string(),
        role: attr("role"),
        status: attr("status"),
        to: attr("to"),
        seq: attr("seq").parse().unwrap_or(0),
        protected: attr("protected") == "true" && r.attrs.contains_key(crate::sensitive::ATTR),
    })
}

#[derive(Default, Clone)]
pub struct Filter {
    pub branch: Option<String>,
    pub agent: Option<String>,
    pub role: Option<String>,
    /// `YYYY-MM-DD`: that day and later.
    pub since: Option<String>,
    pub limit: Option<usize>,
}

/// The entries among `records`, newest first, after the filter.
pub fn entries(records: &[Record], sector: &str, f: &Filter) -> Vec<Entry> {
    let mut out: Vec<Entry> = records
        .iter()
        .filter_map(|r| entry_of(r, sector))
        .filter(|e| f.branch.as_deref().map_or(true, |b| e.branch == b))
        .filter(|e| f.agent.as_deref().map_or(true, |a| e.agent == a))
        .filter(|e| f.role.as_deref().map_or(true, |r| e.role == r))
        .filter(|e| {
            f.since
                .as_deref()
                .map_or(true, |s| e.at.get(..10).unwrap_or(&e.at) >= s)
        })
        .collect();
    sort_newest_first(&mut out);
    if let Some(n) = f.limit {
        out.truncate(n);
    }
    out
}

pub fn sort_newest_first(v: &mut [Entry]) {
    v.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
}

fn who(e: &Entry) -> String {
    if e.protected {
        return "a protected record".to_string();
    }
    match (e.agent.is_empty(), e.reviewed) {
        (true, _) => "a person".to_string(),
        (false, true) => format!("agent `{}`, reviewed by a person", e.agent),
        (false, false) => format!("agent `{}`, not yet reviewed", e.agent),
    }
}

/// One line per entry, for the terminal.
pub fn text(entries: &[Entry]) -> String {
    if entries.is_empty() {
        return "the journal is empty\n".to_string();
    }
    let mut out = String::new();
    for e in entries {
        let when = format!(
            "{} {}",
            e.at.get(..10).unwrap_or(&e.at),
            e.at.get(11..16).unwrap_or("")
        );
        let place = [
            (!e.branch.is_empty()).then(|| e.branch.clone()),
            (!e.commit.is_empty()).then(|| format!("@{}", e.commit)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        out.push_str(&format!("{}  {}  {}\n", when.trim_end(), place, e.summary));
        out.push_str(&format!("    by {}", who(e)));
        if !e.sector.is_empty() {
            out.push_str(&format!(" | sector {}", e.sector));
        }
        out.push('\n');
        if !e.files.is_empty() {
            out.push_str(&format!("    files: {}\n", e.files.join(", ")));
        }
        if !e.why.is_empty() {
            out.push_str(&format!("    {}\n", e.why.replace('\n', "\n    ")));
        }
    }
    out
}

/// The journal as a Markdown page, newest first, grouped by day: what a
/// developer reads on GitHub without installing anything.
pub fn markdown(entries: &[Entry], name: &str) -> String {
    let mut out = format!(
        "# Change journal: {name}\n\n\
         What the agents that worked on this project did, and why, written by them for the \
         developers who read the code later. Newest first. Generated by `fds journal`; \
         edit the entries in the database, not this file.\n"
    );
    if entries.is_empty() {
        out.push_str("\n_No entries yet._\n");
        return out;
    }
    let mut day = String::new();
    for e in entries {
        let d = e.at.get(..10).unwrap_or(&e.at).to_string();
        if d != day {
            out.push_str(&format!("\n## {d}\n"));
            day = d;
        }
        out.push_str(&format!("\n### {}\n\n", e.summary));
        let mut meta = vec![who(e)];
        if let Some(t) = e.at.get(11..16) {
            meta.push(format!("{t} UTC"));
        }
        if !e.branch.is_empty() {
            meta.push(format!("branch `{}`", e.branch));
        }
        if !e.commit.is_empty() {
            meta.push(format!("commit `{}`", e.commit));
        }
        if !e.sector.is_empty() {
            meta.push(format!("sector `{}`", e.sector));
        }
        out.push_str(&format!("{}\n", meta.join(" · ")));
        if !e.why.is_empty() {
            out.push_str(&format!("\n{}\n", e.why));
        }
        if !e.files.is_empty() {
            out.push_str("\nFiles:\n");
            for f in &e.files {
                out.push_str(&format!("- `{f}`\n"));
            }
        }
    }
    out
}

/// One line of the black box: a journal entry, or an event the guard audited.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Step {
    pub at: String,
    /// Position within one database, to keep order inside a second.
    pub order: u64,
    /// `[me]`, `[reasoning]`, `[execution]`, an agent's name, or `guard`.
    pub actor: String,
    pub text: String,
    /// `journal` or `audit`.
    pub from: String,
}

impl Step {
    pub fn of(e: &Entry) -> Step {
        let actor = if !e.role.is_empty() {
            e.role.clone()
        } else if !e.agent.is_empty() {
            e.agent.clone()
        } else {
            "person".to_string()
        };
        let mut text = e.summary.clone();
        if !e.to.is_empty() {
            text.push_str(&format!(" \u{2192} {}", e.to));
        }
        if !e.status.is_empty() {
            text.push_str(&format!(" \u{b7} {}", e.status));
        }
        Step {
            at: e.at.clone(),
            order: e.seq,
            actor,
            text,
            from: "journal".into(),
        }
    }
}

/// Oldest first, which is the order things happened in.
pub fn sort_oldest_first(v: &mut [Step]) {
    v.sort_by(oldest_first);
}

fn oldest_first(a: &Step, b: &Step) -> std::cmp::Ordering {
    a.at.cmp(&b.at)
        .then(a.order.cmp(&b.order))
        .then_with(|| a.text.cmp(&b.text))
}

/// The black box as numbered lines: `#04 [reasoning] technical decision delegated -> decision agent`.
pub fn timeline_text(steps: &[Step]) -> String {
    if steps.is_empty() {
        return "nothing recorded yet\n".to_string();
    }
    let width = steps.len().to_string().len().max(2);
    let mut out = String::new();
    for (i, st) in steps.iter().enumerate() {
        out.push_str(&format!(
            "#{:0w$} [{}] {}\n",
            i + 1,
            st.actor,
            st.text,
            w = width
        ));
    }
    out
}

/// What `fds_journal` answers (SPEC.md §9): the journal of every view a session
/// reads, as text. `views` are `(sector, records)` as the session may see them,
/// that is, already redacted (§11.4); a sector is named in the text only when
/// there is more than one. With `timeline`, the numbered lines of
/// `fds journal show --timeline` (oldest first) and under them the detail of
/// each entry, numbered alike; without it, the format of `fds journal show`
/// (newest first). `f.limit` keeps the newest entries.
pub fn read_text(views: &[(&str, &[Record])], f: &Filter, timeline: bool) -> String {
    let mut all: Vec<Entry> = Vec::new();
    for (name, records) in views {
        let label = if views.len() > 1 { *name } else { "" };
        all.extend(entries(
            records,
            label,
            &Filter {
                limit: None,
                ..f.clone()
            },
        ));
    }
    sort_newest_first(&mut all);
    if let Some(n) = f.limit {
        all.truncate(n);
    }
    if all.is_empty() {
        let filtered =
            f.branch.is_some() || f.agent.is_some() || f.role.is_some() || f.since.is_some();
        return format!(
            "el registro est\u{e1} vac\u{ed}o{}\n",
            if filtered { " con esos filtros" } else { "" }
        );
    }
    if !timeline {
        return text(&all);
    }
    let mut pairs: Vec<(Step, Entry)> = all.into_iter().map(|e| (Step::of(&e), e)).collect();
    pairs.sort_by(|a, b| oldest_first(&a.0, &b.0));
    let steps: Vec<Step> = pairs.iter().map(|p| p.0.clone()).collect();
    let width = steps.len().to_string().len().max(2);
    let mut out = timeline_text(&steps);
    out.push_str("\ndetail:\n");
    for (i, (_, e)) in pairs.iter().enumerate() {
        out.push_str(&format!("#{:0w$} {}\n", i + 1, e.summary, w = width));
        let when = format!(
            "{} {}",
            e.at.get(..10).unwrap_or(&e.at),
            e.at.get(11..16).unwrap_or("")
        );
        let mut facts = vec![format!("by {}", who(e))];
        if !e.protected {
            facts.push(when.trim_end().to_string());
            if !e.branch.is_empty() {
                facts.push(format!("branch {}", e.branch));
            }
            if !e.commit.is_empty() {
                facts.push(format!("commit {}", e.commit));
            }
        }
        if !e.sector.is_empty() {
            facts.push(format!("sector {}", e.sector));
        }
        out.push_str(&format!("    {}\n", facts.join(" | ")));
        if !e.files.is_empty() {
            out.push_str(&format!("    files: {}\n", e.files.join(", ")));
        }
        if !e.why.is_empty() {
            out.push_str(&format!("    why: {}\n", one_line(&e.why, 600)));
        }
    }
    out
}

/// Whitespace folded into single spaces and cut at `max` characters.
fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{cut}\u{2026}")
}

/// The timeline as a Markdown page: a code block, the way a terminal shows it,
/// with the day each run of steps happened on.
pub fn timeline_markdown(steps: &[Step], name: &str) -> String {
    let mut out = format!(
        "# Timeline: {name}\n\nEvery step recorded, oldest first. Generated by `fds journal`; \
         `[brackets]` are who or what took the step.\n\n```\n"
    );
    out.push_str(&timeline_text(steps));
    out.push_str("```\n");
    if let (Some(first), Some(last)) = (steps.first(), steps.last()) {
        out.push_str(&format!("\nFrom {} to {} UTC.\n", first.at, last.at));
    }
    out
}

/// Branch and short commit of the repository in the current directory, or
/// empty strings when there is none. Used to fill in `fds log`.
pub fn git_context() -> (String, String) {
    let here = Path::new(".");
    let branch =
        git(here, &["symbolic-ref", "--quiet", "--short", "HEAD"], None).unwrap_or_default();
    let commit = git(here, &["rev-parse", "--short", "HEAD"], None).unwrap_or_default();
    (branch, commit)
}

fn git(repo: &Path, args: &[&str], input: Option<&[u8]>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run git ({e}); is it installed and on the PATH?"))?;
    if let Some(bytes) = input {
        let mut stdin = child.stdin.take().ok_or("cannot write to git")?;
        stdin
            .write_all(bytes)
            .map_err(|e| format!("cannot write to git: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("git did not finish: {e}"))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            why.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

#[derive(Debug)]
pub struct Published {
    pub commit: String,
    /// The branch already had exactly these files: nothing was committed.
    pub unchanged: bool,
    pub created: bool,
}

/// Commits `files` to `branch` of the repository at `repo` with git's plumbing:
/// the objects are written, the branch reference is moved, and nothing else
/// changes. The working tree, the index and the branch that is checked out stay
/// as they are, so it is safe to run while someone is editing.
pub fn publish(
    repo: &Path,
    branch: &str,
    files: &[(&str, &[u8])],
    message: &str,
) -> Result<Published, String> {
    git(repo, &["rev-parse", "--git-dir"], None)
        .map_err(|_| format!("{} is not a git repository", repo.display()))?;
    let reference = format!("refs/heads/{branch}");
    git(repo, &["check-ref-format", &reference], None)
        .map_err(|_| format!("`{branch}` is not a valid branch name"))?;
    if git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"], None)
        .ok()
        .as_deref()
        == Some(branch)
    {
        return Err(format!("`{branch}` is the branch checked out here; switch to another branch first, or publish from another clone"));
    }

    let mut tree = String::new();
    for (name, bytes) in files {
        let sha = git(repo, &["hash-object", "-w", "--stdin"], Some(bytes))?;
        tree.push_str(&format!("100644 blob {sha}\t{name}\n"));
    }
    let tree = git(repo, &["mktree"], Some(tree.as_bytes()))?;

    let parent = git(
        repo,
        &["rev-parse", "--verify", "--quiet", &reference],
        None,
    )
    .ok()
    .filter(|p| !p.is_empty());
    if let Some(p) = &parent {
        if git(repo, &["rev-parse", &format!("{p}^{{tree}}")], None)? == tree {
            return Ok(Published {
                commit: p.clone(),
                unchanged: true,
                created: false,
            });
        }
    }

    let mut args = vec!["commit-tree", tree.as_str()];
    if let Some(p) = &parent {
        args.extend(["-p", p.as_str()]);
    }
    args.extend(["-m", message]);
    let commit = git(repo, &args, None)?;

    let mut update = vec!["update-ref", reference.as_str(), commit.as_str()];
    if let Some(p) = &parent {
        update.push(p.as_str());
    }
    git(repo, &update, None)?;
    Ok(Published {
        commit,
        unchanged: false,
        created: parent.is_none(),
    })
}

/// The agents and branches present, for a one-line summary.
pub fn summary_line(entries: &[Entry]) -> String {
    let agents: BTreeSet<&str> = entries
        .iter()
        .map(|e| e.agent.as_str())
        .filter(|a| !a.is_empty())
        .collect();
    let branches: BTreeSet<&str> = entries
        .iter()
        .map(|e| e.branch.as_str())
        .filter(|b| !b.is_empty())
        .collect();
    format!(
        "{} entries, {} agent(s), {} branch(es)",
        entries.len(),
        agents.len(),
        branches.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft<'a>(summary: &'a str, at: &'a str, files: &'a [String]) -> Draft<'a> {
        Draft {
            summary,
            why: "",
            files,
            branch: "feat/login",
            commit: "a1b2c3d",
            agent: "backend",
            at,
            ..Default::default()
        }
    }

    #[test]
    fn an_entry_is_a_record_of_type_change() {
        let files = vec![
            "src/auth.rs".to_string(),
            " src/auth.rs ".to_string(),
            "".to_string(),
            "src/lib.rs".to_string(),
        ];
        let mut d = draft(
            "  Add a rate limit\nto /login ",
            "2026-10-21T14:02:11Z",
            &files,
        );
        d.why = "Five failures in a minute lock the address for ten, because the old code allowed unlimited tries.";
        let r = record(&d, "model:backend").unwrap();
        assert_eq!(
            (r.rtype.as_str(), r.kind, r.collection.as_str()),
            ("change", Kind::Knowledge, "journal")
        );
        assert_eq!(r.title, "Add a rate limit to /login");
        assert_eq!(
            r.attrs["files"], "src/auth.rs\nsrc/lib.rs",
            "files are trimmed and not repeated"
        );
        assert_eq!(
            (
                r.verified_by.as_str(),
                r.source.as_str(),
                r.last_verified.as_str()
            ),
            ("model:backend", "agent:backend", "2026-10-21")
        );
        let e = entry_of(&r, "").unwrap();
        assert_eq!(
            (e.agent.as_str(), e.reviewed, e.files.len()),
            ("backend", false, 2)
        );
        assert!(e.why.starts_with("Five failures"));
    }

    #[test]
    fn a_summary_is_required_and_is_one_line() {
        assert!(record(&draft("   ", "2026-10-21T00:00:00Z", &[]), "human").is_err());
        assert!(record(
            &draft(&"x".repeat(201), "2026-10-21T00:00:00Z", &[]),
            "human"
        )
        .is_err());
    }

    #[test]
    fn logging_the_same_thing_twice_in_a_second_keeps_one_entry_and_a_later_one_adds() {
        let a = record(&draft("Fix typo", "2026-10-21T10:00:00Z", &[]), "human").unwrap();
        let b = record(&draft("Fix typo", "2026-10-21T10:00:00Z", &[]), "human").unwrap();
        let c = record(&draft("Fix typo", "2026-10-21T10:00:05Z", &[]), "human").unwrap();
        assert_eq!(a.id, b.id);
        assert_ne!(a.id, c.id);
    }

    #[test]
    fn entries_come_newest_first_and_the_filter_applies() {
        let mk = |s: &str, at: &str, agent: &str, branch: &str| {
            let mut d = draft(s, at, &[]);
            d.agent = agent;
            d.branch = branch;
            record(&d, "model:x").unwrap()
        };
        let mut recs = vec![
            mk("old", "2026-10-19T09:00:00Z", "backend", "a"),
            mk("new", "2026-10-21T09:00:00Z", "frontend", "b"),
            mk("mid", "2026-10-20T09:00:00Z", "backend", "b"),
        ];
        recs.push(Record {
            rtype: String::new(),
            ..mk("not a change", "2026-10-22T00:00:00Z", "x", "x")
        });
        let all = entries(&recs, "", &Filter::default());
        assert_eq!(
            all.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(),
            ["new", "mid", "old"]
        );
        let backend = entries(
            &recs,
            "",
            &Filter {
                agent: Some("backend".into()),
                ..Default::default()
            },
        );
        assert_eq!(backend.len(), 2);
        let recent = entries(
            &recs,
            "",
            &Filter {
                since: Some("2026-10-20".into()),
                limit: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(
            recent
                .iter()
                .map(|e| e.summary.as_str())
                .collect::<Vec<_>>(),
            ["new"]
        );
        let on_b = entries(
            &recs,
            "",
            &Filter {
                branch: Some("b".into()),
                ..Default::default()
            },
        );
        assert_eq!(on_b.len(), 2);
    }

    #[test]
    fn the_markdown_a_developer_reads() {
        let files = vec!["src/auth.rs".to_string()];
        let mut d = draft("Add a rate limit to /login", "2026-10-21T14:02:11Z", &files);
        d.why = "Five failures lock the address for ten minutes.";
        let md = markdown(
            &entries(
                &[record(&d, "model:backend").unwrap()],
                "",
                &Filter::default(),
            ),
            "shop",
        );
        assert!(md.starts_with("# Change journal: shop"));
        for want in [
            "## 2026-10-21",
            "### Add a rate limit to /login",
            "agent `backend`, not yet reviewed · 14:02 UTC · branch `feat/login` · commit `a1b2c3d`",
            "Five failures lock the address for ten minutes.",
            "- `src/auth.rs`",
        ] {
            assert!(md.contains(want), "missing `{want}` in:\n{md}");
        }
        assert!(markdown(&[], "shop").contains("No entries yet"));
    }

    /// The black box of the portfolio picture, written step by step through the
    /// same path an agent uses, and read back in the order it happened.
    #[test]
    fn the_black_box_reads_like_the_picture() {
        let dir = std::env::temp_dir().join(format!("fds-blackbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("box.fds");
        db::save(&path, &mut fds::v4::DbMeta::default(), &[]).unwrap();

        // (role, who, summary, to, status): all in the same second, on purpose.
        let script = [
            ("me", "", "problem recognized", "", ""),
            ("me", "", "architecture defined", "", ""),
            ("me", "", "consultation sent", "reasoning agent", ""),
            (
                "reasoning",
                "claude-r",
                "technical decision delegated",
                "decision agent",
                "",
            ),
            (
                "decision",
                "claude-d",
                "3 tasks assigned",
                "execution agents",
                "",
            ),
            ("execution", "claude-e1", "task 1", "", "running"),
            ("execution", "claude-e2", "task 2", "", "running"),
            ("execution", "claude-e3", "task 3", "", "running"),
            ("execution", "claude-e1", "tasks finished", "", ""),
            ("me", "", "reviewing the changes", "", ""),
        ];
        for (role, agent, summary, to, status) in script {
            let d = Draft {
                summary,
                agent,
                at: "2026-10-23T10:00:00Z",
                role,
                status,
                to,
                ..Default::default()
            };
            let verified = if agent.is_empty() {
                "human".to_string()
            } else {
                format!("model:{agent}")
            };
            append(&path, record(&d, &verified).unwrap()).unwrap();
        }

        let (_, records) = db::load(&path).unwrap();
        let mut steps: Vec<Step> = entries(&records, "", &Filter::default())
            .iter()
            .map(Step::of)
            .collect();
        sort_oldest_first(&mut steps);
        let want = "\
#01 [me] problem recognized
#02 [me] architecture defined
#03 [me] consultation sent \u{2192} reasoning agent
#04 [reasoning] technical decision delegated \u{2192} decision agent
#05 [decision] 3 tasks assigned \u{2192} execution agents
#06 [execution] task 1 \u{b7} running
#07 [execution] task 2 \u{b7} running
#08 [execution] task 3 \u{b7} running
#09 [execution] tasks finished
#10 [me] reviewing the changes
";
        assert_eq!(timeline_text(&steps), want);
        // Written by a person with no agent: `reviewed`; by an agent: waiting.
        let all = entries(&records, "", &Filter::default());
        assert!(all
            .iter()
            .filter(|e| e.agent.is_empty())
            .all(|e| e.reviewed));
        assert!(all
            .iter()
            .filter(|e| !e.agent.is_empty())
            .all(|e| !e.reviewed));
        // Filtering by role keeps the order.
        let exec = entries(
            &records,
            "",
            &Filter {
                role: Some("execution".into()),
                ..Default::default()
            },
        );
        assert_eq!(exec.len(), 4);
        assert!(timeline_markdown(&steps, "demo").contains("```\n#01 [me] problem recognized"));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn have_git() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn temp_repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fds-journal-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q"][..],
            &["symbolic-ref", "HEAD", "refs/heads/main"],
            &["config", "user.name", "tester"],
            &["config", "user.email", "tester@example.com"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(&dir, args, None).unwrap();
        }
        std::fs::write(dir.join("code.txt"), "work in progress\n").unwrap();
        git(&dir, &["add", "code.txt"], None).unwrap();
        git(&dir, &["commit", "-q", "-m", "first"], None).unwrap();
        dir
    }

    #[test]
    fn publishing_moves_only_the_fds_branch() {
        if !have_git() {
            eprintln!("git is not installed: skipped");
            return;
        }
        let repo = temp_repo("publish");
        let head_before = git(&repo, &["rev-parse", "HEAD"], None).unwrap();
        std::fs::write(repo.join("code.txt"), "edited but not committed\n").unwrap();
        std::fs::write(repo.join("scratch.txt"), "untracked\n").unwrap();

        let files: [(&str, &[u8]); 2] = [("JOURNAL.md", b"# one\n"), ("journal.json", b"[]\n")];
        let first = publish(&repo, "fds", &files, "journal: one").unwrap();
        assert!(first.created && !first.unchanged);
        assert_eq!(
            git(&repo, &["show", "fds:JOURNAL.md"], None).unwrap(),
            "# one"
        );

        // The same files again: no new commit.
        let again = publish(&repo, "fds", &files, "journal: one").unwrap();
        assert!(again.unchanged);
        assert_eq!(again.commit, first.commit);

        // Different files: a second commit whose parent is the first.
        let files2: [(&str, &[u8]); 2] = [("JOURNAL.md", b"# two\n"), ("journal.json", b"[]\n")];
        let second = publish(&repo, "fds", &files2, "journal: two").unwrap();
        assert_eq!(
            git(&repo, &["rev-parse", "fds~1"], None).unwrap(),
            first.commit
        );
        assert_eq!(
            git(&repo, &["rev-parse", "fds"], None).unwrap(),
            second.commit
        );

        // Everything the developer had is as it was.
        assert_eq!(
            git(&repo, &["rev-parse", "HEAD"], None).unwrap(),
            head_before
        );
        assert_eq!(
            git(&repo, &["symbolic-ref", "--short", "HEAD"], None).unwrap(),
            "main"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("code.txt")).unwrap(),
            "edited but not committed\n"
        );
        assert!(repo.join("scratch.txt").exists() && !repo.join("JOURNAL.md").exists());
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn publishing_refuses_the_checked_out_branch_and_a_folder_that_is_not_a_repo() {
        if !have_git() {
            return;
        }
        let repo = temp_repo("refuse");
        git(&repo, &["checkout", "-q", "-b", "fds"], None).unwrap();
        let files: [(&str, &[u8]); 1] = [("JOURNAL.md", b"x\n")];
        assert!(publish(&repo, "fds", &files, "m")
            .unwrap_err()
            .contains("checked out"));
        git(&repo, &["checkout", "-q", "main"], None).unwrap();
        assert!(publish(&repo, "bad..name", &files, "m")
            .unwrap_err()
            .contains("not a valid branch name"));
        let plain = std::env::temp_dir().join(format!("fds-journal-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        // A temp dir inside a repo would make this a repository; the system temp is not one.
        if git(&plain, &["rev-parse", "--git-dir"], None).is_err() {
            assert!(publish(&plain, "fds", &files, "m")
                .unwrap_err()
                .contains("not a git repository"));
        }
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(plain);
    }
}
