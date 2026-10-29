//! `fds add` (SPEC.md §7.1): turn files into knowledge records.

use std::path::{Path, PathBuf};

use fds::v4::{Kind, Record};

use crate::text::{stable_id, today, word_count};

const SKIP_DIRS: &[&str] = &["target", "node_modules", ".venv", "venv", "__pycache__", "dist", "build"];
const CODE_EXT: &[&str] = &[
    "rs", "py", "js", "ts", "tsx", "jsx", "go", "java", "c", "h", "cpp", "hpp", "cs", "rb", "php",
    "swift", "kt", "sh", "ps1", "sql", "toml", "yaml", "yml", "json", "html", "css", "vue", "svelte",
];
const TEXT_EXT: &[&str] = &["txt", "rst", "text"];
const MD_EXT: &[&str] = &["md", "markdown"];
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
const CODE_WINDOW: usize = 40;
const TEXT_TARGET_WORDS: usize = 200;

#[derive(Default)]
pub struct Report {
    pub files: usize,
    pub fragments: usize,
    pub skipped: usize,
}

/// Collects every ingestible file under the given paths.
pub fn collect(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        walk(p, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_file() {
        out.push(p.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(p) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(&path, out);
        } else if !name.starts_with('.') {
            out.push(path);
        }
    }
}

/// Forward-slash path relative to the current directory: stable across
/// platforms, so the same file gets the same ids on Windows and Linux.
pub fn source_name(path: &Path) -> String {
    let rel = std::env::current_dir()
        .ok()
        .and_then(|cwd| path.canonicalize().ok().and_then(|abs| abs.strip_prefix(&cwd.canonicalize().ok()?).ok().map(|r| r.to_path_buf())))
        .unwrap_or_else(|| path.to_path_buf());
    rel.to_string_lossy().replace('\\', "/")
}

/// Fragments of one file as (title, body), or None if the file is skipped.
pub fn fragments(path: &Path) -> Option<Vec<(String, String)>> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return None; // binary
    }
    fragments_text(&String::from_utf8_lossy(&bytes), &source_name(path), &ext)
}

/// Fragments of text that did not come from a local file -- an upload in the
/// manager, say. Same splitting as `fragments`, so a document imported from
/// the browser gets exactly the records `fds add` would give it.
/// None if the extension is not one FDS ingests.
pub fn fragments_text(text: &str, name: &str, ext: &str) -> Option<Vec<(String, String)>> {
    let ext = ext.to_lowercase();
    let frags = if MD_EXT.contains(&ext.as_str()) {
        markdown(text, name)
    } else if TEXT_EXT.contains(&ext.as_str()) {
        paragraphs(text, name)
    } else if CODE_EXT.contains(&ext.as_str()) {
        code(text, name)
    } else {
        return None;
    };
    Some(frags.into_iter().filter(|(_, b)| !b.trim().is_empty()).collect())
}

pub const MAX_DOCUMENT_BYTES: usize = MAX_FILE_BYTES as usize;

/// Every extension `fragments_text` accepts. The manager is handed this list
/// rather than keeping its own copy, so the two cannot drift apart.
pub fn document_extensions() -> Vec<&'static str> {
    MD_EXT.iter().chain(TEXT_EXT).chain(CODE_EXT).copied().collect()
}

fn markdown(text: &str, name: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut buf = String::new();
    let mut in_fence = false;

    let heading_path = |stack: &Vec<(usize, String)>| -> String {
        if stack.is_empty() {
            name.to_string()
        } else {
            stack.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > ")
        }
    };

    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        let level = if in_fence { 0 } else { trimmed.chars().take_while(|&c| c == '#').count() };
        if (1..=6).contains(&level) && trimmed.chars().nth(level) == Some(' ') {
            out.push((heading_path(&stack), std::mem::take(&mut buf)));
            let title = trimmed[level..].trim().to_string();
            while stack.last().map_or(false, |(l, _)| *l >= level) {
                stack.pop();
            }
            stack.push((level, title));
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    out.push((heading_path(&stack), buf));
    out.into_iter().map(|(t, b)| (t, b.trim().to_string())).collect()
}

fn paragraphs(text: &str, name: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if !current.is_empty() && word_count(&current) as usize + word_count(para) as usize > TEXT_TARGET_WORDS {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(para);
    }
    if !current.is_empty() {
        out.push(current);
    }
    let n = out.len();
    out.into_iter()
        .enumerate()
        .map(|(i, b)| (if n > 1 { format!("{name} ({}/{n})", i + 1) } else { name.to_string() }, b))
        .collect()
}

fn code(text: &str, name: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .chunks(CODE_WINDOW)
        .enumerate()
        .map(|(i, chunk)| {
            let start = i * CODE_WINDOW + 1;
            (format!("{name}:{}-{}", start, start + chunk.len() - 1), chunk.join("\n"))
        })
        .collect()
}

/// Replaces all knowledge records from `source` with fresh ones.
pub fn upsert(records: &mut Vec<Record>, source: &str, collection: &str, frags: Vec<(String, String)>) -> usize {
    records.retain(|r| !(r.kind == Kind::Knowledge && r.source == source));
    let date = today();
    let n = frags.len();
    for (pos, (title, body)) in frags.into_iter().enumerate() {
        records.push(Record {
            id: stable_id(&format!("knowledge:{source}#{pos}")),
            kind: Kind::Knowledge,
            collection: collection.to_string(),
            question: None,
            tokens: word_count(&body),
            body,
            title,
            source: source.to_string(),
            verified_by: "ingest".to_string(),
            last_verified: date.clone(),
            embedding: None,
            rtype: String::new(),
            attrs: Default::default(),
            links: Vec::new(),
        });
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_splits_by_heading_with_path() {
        let md = "intro\n\n# A\ntext a\n## B\ntext b\n```\n# not a heading\n```\n# C\ntext c\n";
        let f = markdown(md, "doc.md");
        let titles: Vec<&str> = f.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(titles, vec!["doc.md", "A", "A > B", "C"]);
        assert!(f[2].1.contains("# not a heading"), "fenced # must stay in the body");
    }

    #[test]
    fn code_windows_are_40_lines() {
        let src = (1..=85).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let f = code(&src, "x.rs");
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].0, "x.rs:1-40");
        assert_eq!(f[2].0, "x.rs:81-85");
    }

    #[test]
    fn upsert_replaces_previous_fragments() {
        let mut recs = Vec::new();
        upsert(&mut recs, "a.md", "default", vec![("t".into(), "one".into()), ("t".into(), "two".into())]);
        upsert(&mut recs, "a.md", "default", vec![("t".into(), "three".into())]);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].body, "three");
    }
}
