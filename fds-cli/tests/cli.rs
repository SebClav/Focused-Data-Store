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

/// Runs `fds mcp --sectors` for the `sales` agent, feeds it the lines on stdin
/// and returns the parsed replies.
fn mcp_sectors(sandbox: &Sandbox, global: &[&str], lines: &[&str]) -> Vec<serde_json::Value> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_fds"))
        .current_dir(&sandbox.0)
        .args(global)
        .args(["mcp", "--sectors", "sectors.json", "--as", "sales"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for l in lines {
        writeln!(stdin, "{l}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    text(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

const REMEMBER: &str = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fds_remember","arguments":{"question":"client acme prefers","answer":"invoices in USD"}}}"#;
const LIST: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

#[test]
fn read_only_mcp_with_sectors_cannot_write() {
    let sandbox = Sandbox::new();
    ok(&sandbox.fds(&["sector", "new", "sales", "--agent"]));
    let before = fs::read(sandbox.path("sales.fds")).unwrap();

    let replies = mcp_sectors(&sandbox, &["--read-only"], &[REMEMBER, LIST]);
    assert_eq!(replies[0]["result"]["isError"], true, "{}", replies[0]);
    let tools = replies[1]["result"]["tools"].as_array().unwrap();
    assert!(
        !tools
            .iter()
            .any(|t| ["fds_remember", "fds_log", "fds_forget"]
                .contains(&t["name"].as_str().unwrap())),
        "{tools:?}"
    );
    assert_eq!(fs::read(sandbox.path("sales.fds")).unwrap(), before);
}

#[test]
fn mcp_with_sectors_still_writes_without_read_only() {
    let sandbox = Sandbox::new();
    ok(&sandbox.fds(&["sector", "new", "sales", "--agent"]));
    let replies = mcp_sectors(&sandbox, &[], &[REMEMBER]);
    assert!(replies[0]["result"]["isError"].is_null(), "{}", replies[0]);
    let out = sandbox.fds(&["--db", "sales.fds", "query", "client acme prefers"]);
    assert!(text(&out.stdout).contains("invoices in USD"));
}

#[test]
fn a_fact_that_replaces_another_collections_answer_says_so() {
    let sandbox = Sandbox::new();
    ok(&sandbox.fds(&["init", "t"]));
    ok(&sandbox.fds(&[
        "--db",
        "t.fds",
        "fact",
        "add",
        "What is the port?",
        "443",
        "-c",
        "alpha",
    ]));

    let out = sandbox.fds(&[
        "--db",
        "t.fds",
        "fact",
        "add",
        "What is the port?",
        "8080",
        "-c",
        "beta",
    ]);
    ok(&out);
    let warning = text(&out.stderr);
    assert!(warning.contains("warning: fact"), "{warning}");
    assert!(
        warning.contains("`alpha`") && warning.contains("`beta`"),
        "{warning}"
    );

    // The identifier comes from the question alone, as SPEC.md 3.1 says: one fact.
    let listing = text(&sandbox.fds(&["--db", "t.fds", "ls"]).stdout);
    assert_eq!(listing.matches("what is the port").count(), 1, "{listing}");
    assert!(listing.contains("beta"), "{listing}");
}

#[test]
fn repeating_a_fact_unchanged_does_not_warn() {
    let sandbox = Sandbox::with_database();
    let out = sandbox.fds(&["--db", "t.fds", "fact", "add", "What is the port?", "443"]);
    ok(&out);
    assert!(text(&out.stderr).is_empty(), "{}", text(&out.stderr));
}

#[test]
fn importing_a_fact_that_replaces_another_warns_for_each() {
    let sandbox = Sandbox::with_database();
    fs::write(
        sandbox.path("facts.json"),
        r#"[{"question": "What is the port?", "answer": "8080"},
            {"question": "What is the host?", "answer": "example.test"}]"#,
    )
    .unwrap();
    let out = sandbox.fds(&["--db", "t.fds", "fact", "import", "facts.json"]);
    ok(&out);
    let stderr = text(&out.stderr);
    assert_eq!(stderr.matches("warning: fact").count(), 1, "{stderr}");
    assert!(stderr.contains("Imported 2 facts"), "{stderr}");
}

#[test]
fn a_negated_question_is_not_an_exact_match() {
    let sandbox = Sandbox::new();
    ok(&sandbox.fds(&["init", "t"]));
    ok(&sandbox.fds(&[
        "--db",
        "t.fds",
        "fact",
        "add",
        "Can users delete records?",
        "Yes",
    ]));

    let confidence = |question: &str| {
        let out = sandbox.fds(&["--db", "t.fds", "--json", "query", question]);
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        json["confidence"].as_str().unwrap().to_string()
    };
    assert_eq!(confidence("can users delete records"), "exact");
    for negated in [
        "Cannot users delete records?",
        "Users can not delete records",
        "Can't users delete records?",
        "Do users not delete records?",
    ] {
        assert_ne!(confidence(negated), "exact", "{negated}");
    }
}

#[test]
fn a_different_question_word_or_modal_is_not_an_exact_match() {
    let sandbox = Sandbox::new();
    ok(&sandbox.fds(&["init", "t"]));
    ok(&sandbox.fds(&[
        "--db",
        "t.fds",
        "fact",
        "add",
        "Can I delete the account?",
        "Yes",
    ]));

    let confidence = |question: &str| {
        let out = sandbox.fds(&["--db", "t.fds", "--json", "query", question]);
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        json["confidence"].as_str().unwrap().to_string()
    };
    assert_eq!(confidence("can i delete the account"), "exact");
    for other in [
        "Should I delete the account?",
        "Who can delete the account?",
        "How can I delete the account?",
        "When can I delete the account?",
    ] {
        assert_ne!(confidence(other), "exact", "{other}");
    }
}
