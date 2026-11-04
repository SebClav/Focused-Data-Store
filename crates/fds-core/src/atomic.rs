//! Replacing a file so that a reader never sees it half written (SPEC.md §6.1).

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::FdsError;

/// Writes `bytes` to a temporary file of its own, flushes it to disk, and
/// renames it over `path`.
///
/// The temporary name is unique per write. With one shared name, two writers
/// that ever got here together would truncate each other's file.
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), FdsError> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.{nanos}.{seq}.tmp", std::process::id()));
    let tmp = PathBuf::from(name);
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    replace(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.into()
    })
}

/// Renames `tmp` over `path`. On Windows a reader that has the file open or
/// mapped makes the replace fail for the few milliseconds it holds it
/// (sharing violation, or a user-mapped section), so those are retried.
fn replace(tmp: &Path, path: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match fs::rename(tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if tries < 150 && transient(&e) => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
}

fn transient(e: &std::io::Error) -> bool {
    // 5 access denied, 32 sharing violation, 1224 user-mapped section open.
    e.kind() == std::io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32 | 1224))
}
