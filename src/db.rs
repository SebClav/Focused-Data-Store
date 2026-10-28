//! Locating, locking, loading and saving the database file.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fds::v4::{self, DbMeta, Record};
use fds::FdsError;

use crate::record::{same_identity, today};

/// A database loaded into memory, together with the file it came from.
pub struct Store {
    pub path: PathBuf,
    pub meta: DbMeta,
    pub records: Vec<Record>,
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let (meta, records) =
            v4::read_db(&path).map_err(|e| format!("{}: {}", path.display(), explain(e)))?;
        Ok(Store {
            path,
            meta,
            records,
        })
    }

    /// Update database metadata and persist the complete record set.
    pub fn save(&mut self) -> Result<(), String> {
        self.meta.updated = today();
        v4::write_db(&self.path, &self.meta, &self.records).map_err(explain)
    }

    /// Save a record, replacing the one it is (see [`same_identity`]) and
    /// keeping that record's ID. Returns the ID the record now has.
    pub fn upsert(&mut self, mut record: Record) -> String {
        let existing = self.records.iter().find(|x| same_identity(x, &record));
        if let Some(existing) = existing {
            record.id = existing.id.clone();
        }
        self.records
            .retain(|x| x.id != record.id && !same_identity(x, &record));
        let id = record.id.clone();
        self.records.push(record);
        id
    }
}

fn explain(e: FdsError) -> String {
    format!("{e:?}")
}

/// Create an empty FDS v4 database, adding the file extension when omitted.
pub fn init(name: &str) -> Result<u8, String> {
    let mut path = PathBuf::from(name);
    if path.extension().is_none() {
        path.set_extension("fds");
    }
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let today = today();
    let meta = DbMeta {
        name: path
            .file_stem()
            .and_then(|x| x.to_str())
            .unwrap_or("knowledge")
            .into(),
        created: today.clone(),
        updated: today,
        embedding_model: String::new(),
        embedding_dims: 0,
    };
    v4::write_db(&path, &meta, &[]).map_err(explain)?;
    println!("created {}", path.display());
    Ok(0)
}

/// Select the requested database or infer it only when exactly one `.fds` file exists.
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        return Ok(path.into());
    }
    let mut files: Vec<_> = fs::read_dir(".")
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "fds"))
        .collect();
    files.sort();
    match files.len() {
        0 => Err("no database here; run `fds init` or pass --db".into()),
        1 => Ok(files.remove(0)),
        _ => Err("multiple .fds databases found; select one with --db".into()),
    }
}

/// Hold the writer lock from read through atomic replacement. This prevents
/// two FDS processes from silently overwriting each other's changes.
pub fn lock_database(path: &Path) -> Result<fs::File, String> {
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("{}: {e}", lock_path.display()))?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if start.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(format!("{} is busy; writer lock timed out", path.display()))
            }
            Err(fs::TryLockError::Error(e)) => {
                return Err(format!("cannot lock {}: {e}", path.display()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use fds::v4::{DbMeta, Kind, Record};
    use std::path::PathBuf;

    use super::Store;
    use crate::record::{fact, record};

    fn store() -> Store {
        Store {
            path: PathBuf::new(),
            meta: DbMeta::default(),
            records: Vec::new(),
        }
    }

    #[test]
    fn the_same_question_in_two_collections_is_two_records() {
        let mut store = store();
        let alpha = store.upsert(fact("What is the port?", "443", "alpha", "s1").unwrap());
        let beta = store.upsert(fact("What is the port?", "8080", "beta", "s2").unwrap());

        assert_ne!(alpha, beta);
        assert_eq!(store.records.len(), 2);
        let body = |id: &str| {
            store
                .records
                .iter()
                .find(|r| r.id == id)
                .unwrap()
                .body
                .clone()
        };
        assert_eq!(body(&alpha), "443");
        assert_eq!(body(&beta), "8080");
    }

    #[test]
    fn the_same_question_in_the_same_collection_is_replaced() {
        let mut store = store();
        let first = store.upsert(fact("What is the port?", "443", "alpha", "s1").unwrap());
        let second = store.upsert(fact("what is the PORT", "8080", "alpha", "s2").unwrap());

        assert_eq!(first, second);
        assert_eq!(store.records.len(), 1);
        assert_eq!(store.records[0].body, "8080");
    }

    #[test]
    fn replacing_a_record_with_an_older_id_keeps_that_id() {
        // Databases written before the collection was part of the ID.
        let mut old: Record = fact("What is the port?", "443", "alpha", "s1").unwrap();
        old.id = "00000000deadbeef".into();
        let mut store = store();
        store.records.push(old);

        let id = store.upsert(fact("What is the port?", "8080", "alpha", "s2").unwrap());

        assert_eq!(id, "00000000deadbeef");
        assert_eq!(store.records.len(), 1);
        assert_eq!(store.records[0].body, "8080");
    }

    #[test]
    fn knowledge_from_one_source_is_kept_per_collection() {
        let mut store = store();
        let add = |store: &mut Store, collection: &str, text: &str| {
            store.upsert(record(
                Kind::Knowledge,
                collection,
                None,
                text.into(),
                "doc",
                "notes.txt",
                "ingest",
                "",
            ))
        };
        let a = add(&mut store, "kb-a", "first");
        let b = add(&mut store, "kb-b", "second");
        add(&mut store, "kb-a", "first, revised");

        assert_ne!(a, b);
        assert_eq!(store.records.len(), 2);
    }
}
