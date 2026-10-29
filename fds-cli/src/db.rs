//! Locating, loading and saving a database file.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use fds::v4::{read_db, write_db, DbMeta, Record};
use fds::FdsError;

use crate::text::today;

/// `--db <file>` if given, else the single `*.fds` file in the current
/// directory (SPEC.md §7) -- the way git finds "the repo here".
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(".")
        .map_err(|e| format!("cannot read current directory: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().map_or(false, |x| x == "fds"))
        .collect();
    found.sort();
    match found.len() {
        0 => Err("no database here. Create one with `fds init`, or pass --db <file>.".into()),
        1 => Ok(found.remove(0)),
        _ => Err(format!(
            "several databases here ({}). Pick one with --db <file>.",
            found
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub fn load(path: &Path) -> Result<(DbMeta, Vec<Record>), String> {
    read_db(path).map_err(|e| describe(path, e))
}

/// Held while a write reads, changes and saves the file; released when it
/// goes out of scope. Every writer takes it -- the CLI, the server, an agent
/// through MCP -- so two of them can never both start from the same old copy
/// and the second erase the first (SPEC.md §6.7).
pub struct WriteLock {
    _file: std::fs::File,
}

/// Waits for the database's write lock. A write that has held it longer than
/// `FDS_LOCK_TIMEOUT_SECS` (default 60) makes this one give up with a clear
/// message instead of hanging.
pub fn lock(path: &Path) -> Result<WriteLock, String> {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let lock_path = PathBuf::from(name);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    let wait = Duration::from_secs(
        std::env::var("FDS_LOCK_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60),
    );
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(WriteLock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < wait => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "{} is busy: another write has held it for {}s. Try again.",
                    path.display(),
                    wait.as_secs()
                ))
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!("{}: cannot lock: {e}", lock_path.display()))
            }
        }
    }
}

/// What a reader keeps to notice that someone else rewrote the file: a write
/// always replaces it, so the modification time moves even when the size
/// does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some(Stamp {
        len: m.len(),
        modified: m.modified().ok(),
    })
}

pub fn save(path: &Path, meta: &mut DbMeta, records: &[Record]) -> Result<(), String> {
    meta.updated = today();
    write_db(path, meta, records).map_err(|e| describe(path, e))
}

fn describe(path: &Path, e: FdsError) -> String {
    let name = path.display();
    match e {
        FdsError::BadMagic => {
            let head = std::fs::read(path)
                .ok()
                .map(|b| b.into_iter().take(4).collect::<Vec<_>>());
            if head.as_deref() == Some(b"FDS1") {
                format!("{name} is a version 1 file; this tool reads versions 2, 3 and 4. Re-create it with `fds init` and `fds add`.")
            } else {
                format!("{name} is not a FDS database.")
            }
        }
        FdsError::UnsupportedVersion(v) => {
            format!("{name} uses format version {v}, which this tool does not support.")
        }
        FdsError::Truncated => format!("{name} is truncated or corrupt."),
        FdsError::Io(e) => format!("{name}: {e}"),
        FdsError::Bincode(e) => format!("{name} is corrupt: {e}"),
        FdsError::BadChecksum(region) => {
            format!("{name} is damaged: the checksum of its {region} does not match. Restore it from a copy, or export what is readable.")
        }
        FdsError::NewerFormat { major, minor } => {
            format!("{name} was written by format {major}.{minor}, newer than this tool. It can be read, but saving would drop fields this tool does not know; update the tool to change it.")
        }
        FdsError::Malformed(why) => format!("{name} is corrupt: {why}."),
    }
}

/// Accepts a full id or an unambiguous prefix of at least 6 characters.
pub fn resolve_id(records: &[Record], wanted: &str) -> Result<usize, String> {
    if let Some(i) = records.iter().position(|r| r.id == wanted) {
        return Ok(i);
    }
    if wanted.len() < 6 {
        return Err(format!(
            "id prefix `{wanted}` is too short; use at least 6 characters."
        ));
    }
    let matches: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.id.starts_with(wanted))
        .map(|(i, _)| i)
        .collect();
    match matches.len() {
        0 => Err(format!("no record with id `{wanted}`.")),
        1 => Ok(matches[0]),
        n => Err(format!(
            "id prefix `{wanted}` matches {n} records; use more characters."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The audit's failing case: several writers at once. Each thread opens
    /// its own lock handle, exactly as separate processes do.
    #[test]
    fn concurrent_writers_keep_every_record() {
        let dir = std::env::temp_dir().join(format!("fds-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lock.fds");
        save(&path, &mut DbMeta::default(), &[]).unwrap();
        let writers: Vec<_> = (0..4)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..25 {
                        let _lock = lock(&path).unwrap();
                        let (mut meta, mut records) = load(&path).unwrap();
                        crate::ops::upsert_fact(
                            &mut records,
                            &format!("question {t} {i}"),
                            "answer",
                            "default",
                            "test",
                            "human",
                            "2026-10-12",
                        )
                        .unwrap();
                        save(&path, &mut meta, &records).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        assert_eq!(load(&path).unwrap().1.len(), 100);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temporary file is left behind");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_stamp_moves_when_the_file_is_rewritten() {
        let path = std::env::temp_dir().join(format!("fds-stamp-{}.fds", std::process::id()));
        save(&path, &mut DbMeta::default(), &[]).unwrap();
        let before = stamp(&path);
        std::thread::sleep(Duration::from_millis(20));
        save(&path, &mut DbMeta::default(), &[]).unwrap();
        assert_ne!(
            before,
            stamp(&path),
            "same size, new file: the stamp must differ"
        );
        std::fs::remove_file(&path).ok();
    }
}
