use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

struct TestDb(PathBuf);

impl TestDb {
    fn new() -> Self {
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join("review-tests");
        fs::create_dir_all(&dir).unwrap();
        let seq = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        Self(dir.join(format!("review-{}-{seq}.fds", std::process::id())))
    }

    fn init(&self) {
        let out = Command::new(env!("CARGO_BIN_EXE_fds"))
            .arg("init")
            .arg(&self.0)
            .output()
            .unwrap();
        assert_success(&out);
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fds"))
            .arg("--db")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let mut lock = self.0.as_os_str().to_owned();
        lock.push(".lock");
        let _ = fs::remove_file(PathBuf::from(lock));
        for extension in ["facts.json", "proc.json", "txt"] {
            let _ = fs::remove_file(self.0.with_extension(extension));
        }
    }
}

fn assert_success(out: &Output) {
    assert!(
        out.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn json(out: &Output) -> Value {
    assert_success(out);
    serde_json::from_slice(&out.stdout).unwrap()
}

fn database_status(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fds"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn proposal_requires_explicit_review_and_replacement_resets_approval() {
    let db = TestDb::new();
    db.init();

    let added = db.run(&["fact", "add", "What is the service port?", "443"]);
    assert_success(&added);
    let id = String::from_utf8(added.stdout).unwrap().trim().to_string();
    let pending = json(&db.run(&["--json", "show", &id]));
    assert_eq!(pending["review_status"], "pending");
    assert_eq!(pending["verified_by"], "unreviewed");
    assert_eq!(pending["last_verified"], "");

    let hidden = db.run(&[
        "--json",
        "query",
        "What is the service port?",
        "--review-status",
        "approved",
    ]);
    assert_eq!(hidden.status.code(), Some(2));
    let waiting = json(&db.run(&["--read-only", "--json", "review", "list"]));
    assert_eq!(waiting.as_array().unwrap().len(), 1);
    assert_eq!(waiting[0]["id"], id);

    let before = fs::read(&db.0).unwrap();
    let blocked = db.run(&["--read-only", "review", "approve", &id, "--reviewer", "Ada"]);
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("read-only"));
    assert_eq!(fs::read(&db.0).unwrap(), before);

    let missing_reviewer = db.run(&["review", "approve", &id, "--reviewer", "   "]);
    assert!(!missing_reviewer.status.success());
    assert_eq!(fs::read(&db.0).unwrap(), before);

    let approved = json(&db.run(&[
        "--json",
        "review",
        "approve",
        &id,
        "--reviewer",
        "Ada",
        "--note",
        "Checked against spec",
    ]));
    assert_eq!(approved["review_status"], "approved");
    assert_eq!(approved["verified_by"], "human");
    assert_eq!(approved["attrs"]["fds.review.reviewer"], "Ada");
    assert_eq!(approved["attrs"]["fds.review.note"], "Checked against spec");
    assert!(!approved["last_verified"].as_str().unwrap().is_empty());

    let visible = json(&db.run(&[
        "--json",
        "query",
        "What is the service port?",
        "--review-status",
        "approved",
    ]));
    assert_eq!(visible["results"][0]["id"], id);

    assert_success(&db.run(&["fact", "add", "What is the service port?", "443"]));
    let replaced = json(&db.run(&["--json", "show", &id]));
    assert_eq!(replaced["review_status"], "pending");
    assert_eq!(replaced["verified_by"], "unreviewed");
    assert!(replaced["attrs"].get("fds.review.reviewer").is_none());
}

#[test]
fn legacy_human_labels_are_pending_until_reviewed() {
    let db = TestDb::new();
    fs::copy("examples/agent_guide.fds", &db.0).unwrap();
    let pending = json(&database_status(
        &db.0,
        &["--read-only", "--json", "review", "list"],
    ));
    let records = pending.as_array().unwrap();
    assert!(!records.is_empty());
    assert!(records.iter().all(|r| r["review_status"] == "pending"));
    assert!(records.iter().any(|r| r["verified_by"] == "human"));
    let approved = json(&database_status(
        &db.0,
        &[
            "--read-only",
            "--json",
            "review",
            "list",
            "--status",
            "approved",
        ],
    ));
    assert!(approved.as_array().unwrap().is_empty());
}

#[test]
fn imported_facts_procedures_contexts_and_documents_start_pending() {
    let db = TestDb::new();
    db.init();
    let facts = db.0.with_extension("facts.json");
    let procedure = db.0.with_extension("proc.json");
    let document = db.0.with_extension("txt");
    fs::write(
        &facts,
        r#"[{"question":"What is the hostname?","answer":"example.test"}]"#,
    )
    .unwrap();
    fs::write(
        &procedure,
        r#"{"intent":"Deploy service","steps":[{"action":"review"}]}"#,
    )
    .unwrap();
    fs::write(&document, "A document supplied by an agent.").unwrap();

    assert_success(&db.run(&["fact", "import", facts.to_str().unwrap()]));
    assert_success(&db.run(&["proc", "add", procedure.to_str().unwrap()]));
    assert_success(&db.run(&["context", "set", "rules", "--instructions", "Ask a human"]));
    assert_success(&db.run(&["add", document.to_str().unwrap()]));

    let listed = json(&db.run(&["--read-only", "--json", "review", "list", "--status", "all"]));
    let records = listed.as_array().unwrap();
    assert_eq!(records.len(), 4);
    assert!(records.iter().all(|r| r["review_status"] == "pending"));
    assert!(records.iter().all(|r| r["last_verified"] == ""));
    // A claim of `human` is withdrawn, but where a record came from is kept.
    let label = |title: &str| {
        records
            .iter()
            .find(|r| r["title"].as_str().is_some_and(|t| t.contains(title)))
            .unwrap()["verified_by"]
            .clone()
    };
    assert_eq!(label("hostname"), "unreviewed");
    assert_eq!(label("Deploy"), "unreviewed");
    assert_eq!(label("rules"), "unreviewed");
    assert_eq!(label(".txt"), "ingest");
}

#[test]
fn approval_is_scoped_to_the_record_in_its_collection() {
    let db = TestDb::new();
    db.init();
    let alpha = db.run(&[
        "fact",
        "add",
        "What is the port?",
        "443",
        "--collection",
        "alpha",
    ]);
    let beta = db.run(&[
        "fact",
        "add",
        "What is the port?",
        "8080",
        "--collection",
        "beta",
    ]);
    assert_success(&alpha);
    assert_success(&beta);
    let alpha_id = String::from_utf8(alpha.stdout).unwrap().trim().to_owned();
    let beta_id = String::from_utf8(beta.stdout).unwrap().trim().to_owned();
    assert_ne!(alpha_id, beta_id);
    assert_success(&db.run(&["review", "approve", &alpha_id, "--reviewer", "Ada"]));

    let approved = json(&db.run(&[
        "--read-only",
        "--json",
        "query",
        "What is the port?",
        "--review-status",
        "approved",
    ]));
    let rows = approved["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], alpha_id);
    assert_eq!(rows[0]["collection"], "alpha");

    let pending = json(&db.run(&["--read-only", "--json", "review", "list"]));
    let rows = pending.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], beta_id);
    assert_eq!(rows[0]["collection"], "beta");
}

#[test]
fn ingested_documents_keep_their_provenance_label_and_stay_filterable() {
    let db = TestDb::new();
    db.init();
    let document = db.0.with_extension("txt");
    fs::write(&document, "Billing runbook. Restart nightly.").unwrap();
    assert_success(&db.run(&["add", document.to_str().unwrap(), "-c", "kb"]));

    let found = json(&db.run(&["--json", "query", "billing", "--verified-by", "ingest"]));
    assert_eq!(found["results"][0]["verified_by"], "ingest");
    assert_eq!(found["results"][0]["review_status"], "pending");

    let id = found["results"][0]["id"].as_str().unwrap().to_string();
    let approved = json(&db.run(&["--json", "review", "approve", &id, "--reviewer", "Ada"]));
    assert_eq!(approved["verified_by"], "human");
    assert_eq!(approved["attrs"]["fds.origin"], "ingest");
}

#[test]
fn a_query_says_how_many_results_are_pending_review() {
    let db = TestDb::new();
    db.init();
    assert_success(&db.run(&["fact", "add", "Who owns billing?", "Team A"]));

    let out = db.run(&["--json", "query", "Who owns billing?"]);
    assert_success(&out);
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["confidence"], "exact");
    assert_eq!(result["pending_review"], 1);
    let note = String::from_utf8_lossy(&out.stderr);
    assert!(
        note.contains("1 of 1 result(s) are pending human review"),
        "{note}"
    );

    let id = result["results"][0]["id"].as_str().unwrap().to_string();
    assert_success(&db.run(&["review", "approve", &id, "--reviewer", "Ada"]));
    let out = db.run(&["--json", "query", "Who owns billing?"]);
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["pending_review"], 0);
    assert!(out.stderr.is_empty());

    // Asking for a status explicitly is not nagged about.
    assert_success(&db.run(&["fact", "add", "Who owns billing?", "Team B"]));
    let out = db.run(&[
        "--json",
        "query",
        "Who owns billing?",
        "--review-status",
        "pending",
    ]);
    assert!(out.stderr.is_empty());
}
