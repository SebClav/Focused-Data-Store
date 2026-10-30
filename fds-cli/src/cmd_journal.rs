//! `fds journal`: read, export or publish the change journal.

use std::path::Path;

use serde_json::json;

use crate::cli::{JournalCmd, JournalSrc};
use crate::{audit, db, journal, print_json, sectors};

/// The entries a `journal` command works on, newest first, and a name for the page.
pub fn journal_entries(
    src: &JournalSrc,
    db_file: Option<&Path>,
) -> Result<(Vec<journal::Entry>, String), String> {
    let filter = journal::Filter {
        branch: src.branch.clone(),
        agent: src.agent.clone(),
        role: src.role.clone(),
        since: src.since.clone(),
        limit: None,
    };
    let mut all = Vec::new();
    let name;
    if let Some(manifest) = &src.sectors {
        let m = sectors::Manifest::load(manifest)?;
        name = "all sectors".to_string();
        for def in &m.sectors {
            let (_, records) = db::load(&m.path_of(def))?;
            all.extend(journal::entries(&records, &def.name, &filter));
        }
    } else {
        let path = db::locate(db_file)?;
        let (meta, records) = db::load(&path)?;
        name = if meta.name.is_empty() {
            path.display().to_string()
        } else {
            meta.name
        };
        all = journal::entries(&records, "", &filter);
    }
    journal::sort_newest_first(&mut all);
    if let Some(n) = src.limit {
        all.truncate(n);
    }
    Ok((all, name))
}

/// The black box: journal entries and, when there is an audit file, the guard's
/// decisions, in the order they happened.
pub fn journal_steps(
    src: &JournalSrc,
    entries: &[journal::Entry],
) -> Result<Vec<journal::Step>, String> {
    let mut steps: Vec<journal::Step> = entries.iter().map(journal::Step::of).collect();
    let audit_file = src.audit.clone().or_else(|| {
        src.sectors
            .as_deref()
            .map(audit::beside)
            .filter(|p| p.exists())
    });
    if let Some(f) = audit_file {
        steps.extend(audit::steps(&f)?);
    }
    journal::sort_oldest_first(&mut steps);
    Ok(steps)
}

pub fn journal_cmd(cmd: &JournalCmd, db_file: Option<&Path>, json_out: bool) -> Result<u8, String> {
    match cmd {
        JournalCmd::Show { src, md, timeline } => {
            let (entries, name) = journal_entries(src, db_file)?;
            if *timeline {
                let steps = journal_steps(src, &entries)?;
                if json_out {
                    print_json(&serde_json::to_value(&steps).map_err(|e| e.to_string())?);
                } else if *md {
                    print!("{}", journal::timeline_markdown(&steps, &name));
                } else {
                    print!("{}", journal::timeline_text(&steps));
                }
            } else if json_out {
                print_json(&serde_json::to_value(&entries).map_err(|e| e.to_string())?);
            } else if *md {
                print!("{}", journal::markdown(&entries, &name));
            } else {
                print!("{}", journal::text(&entries));
            }
        }
        JournalCmd::Export { src, out } => {
            let (entries, name) = journal_entries(src, db_file)?;
            std::fs::write(out, journal::markdown(&entries, &name))
                .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
            eprintln!(
                "Wrote {} to {}.",
                journal::summary_line(&entries),
                out.display()
            );
        }
        JournalCmd::Publish { src, repo, to } => {
            let (entries, name) = journal_entries(src, db_file)?;
            let md = journal::markdown(&entries, &name);
            let js = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())? + "\n";
            let message = format!("journal: {}", journal::summary_line(&entries));
            let steps = journal_steps(src, &entries)?;
            let tl = journal::timeline_markdown(&steps, &name);
            let files: [(&str, &[u8]); 3] = [
                ("JOURNAL.md", md.as_bytes()),
                ("TIMELINE.md", tl.as_bytes()),
                ("journal.json", js.as_bytes()),
            ];
            let done = journal::publish(repo, to, &files, &message)?;
            let short = &done.commit[..done.commit.len().min(10)];
            if done.unchanged {
                eprintln!("Branch `{to}` already has this journal ({short}); nothing to commit.");
            } else {
                eprintln!(
                    "{} branch `{to}` at {short} with {}.",
                    if done.created { "Created" } else { "Updated" },
                    journal::summary_line(&entries)
                );
                eprintln!("Your files, your index and your current branch were not touched. To share it: git push origin {to}");
            }
            if json_out {
                print_json(
                    &json!({"branch": to, "commit": done.commit, "unchanged": done.unchanged}),
                );
            }
        }
    }
    Ok(0)
}
