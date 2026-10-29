//! Runs the `fds` binary in a scratch directory.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch directory, removed when dropped.
struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fds-cli-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Sandbox(dir)
    }

    /// Runs `fds` with the arguments, in the scratch directory.
    fn fds(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fds"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }

    /// A database with one fact, made with the writing commands.
    fn with_database() -> Self {
        let sandbox = Sandbox::new();
        ok(&sandbox.fds(&["init", "t"]));
        ok(&sandbox.fds(&["--db", "t.fds", "fact", "add", "What is the port?", "443"]));
        sandbox
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ok(out: &Output) {
    assert!(out.status.success(), "failed: {}", text(&out.stderr));
}

#[test]
fn read_only_refuses_every_write_and_leaves_the_file_untouched() {
    let sandbox = Sandbox::with_database();
    let before = fs::read(sandbox.path("t.fds")).unwrap();
    fs::write(sandbox.path("note.txt"), "a note").unwrap();

    let attempts: &[&[&str]] = &[
        &["fact", "add", "Another question?", "yes"],
        &["add", "note.txt"],
        &["context", "set", "rules", "--instructions", "Ask a human"],
        &["log", "changed something"],
        &["rm", "--source", "manual"],
        &["embed"],
    ];
    for attempt in attempts {
        let mut args = vec!["--read-only", "--db", "t.fds"];
        args.extend_from_slice(attempt);
        let out = sandbox.fds(&args);
        assert_eq!(out.status.code(), Some(1), "{attempt:?} was not refused");
        assert!(
            text(&out.stderr).contains("--read-only prevents"),
            "{attempt:?}"
        );
    }
    assert_eq!(fs::read(sandbox.path("t.fds")).unwrap(), before);
}

#[test]
fn read_only_does_not_create_a_database_or_a_sector_manifest() {
    let sandbox = Sandbox::new();
    assert_eq!(
        sandbox.fds(&["--read-only", "init", "new"]).status.code(),
        Some(1)
    );
    assert!(!sandbox.path("new.fds").exists());

    let out = sandbox.fds(&["--read-only", "sector", "new", "sales"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!sandbox.path("sectors.json").exists());
}

#[test]
fn read_only_still_reads() {
    let sandbox = Sandbox::with_database();
    for args in [
        &["status"][..],
        &["ls"],
        &["query", "port"],
        &["context", "ls"],
        &["journal", "show"],
        &["policy", "ls"],
    ] {
        let mut full = vec!["--read-only", "--db", "t.fds"];
        full.extend_from_slice(args);
        let out = sandbox.fds(&full);
        assert!(
            out.status.success(),
            "{args:?}: {}{}",
            text(&out.stdout),
            text(&out.stderr)
        );
    }
}

#[test]
fn mcp_is_read_only_unless_asked_to_write() {
    let sandbox = Sandbox::with_database();
    let out = sandbox.fds(&["--read-only", "--db", "t.fds", "mcp", "--write"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("--read-only prevents"));
}
