//! Runs the `fds` binary against a throwaway database.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A scratch directory holding one database, removed when dropped.
struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fds-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let sandbox = Sandbox(dir);
        let init = Command::new(env!("CARGO_BIN_EXE_fds"))
            .current_dir(&sandbox.0)
            .args(["init", "t"])
            .output()
            .unwrap();
        assert!(init.status.success());
        sandbox
    }

    fn fds(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_fds"))
            .current_dir(&self.0)
            .args(["--db", "t.fds"])
            .args(args)
            .output()
            .unwrap()
    }

    fn fact(&self, question: &str, answer: &str, collection: &str) {
        let out = self.fds(&["fact", "add", question, answer, "-c", collection]);
        assert!(out.status.success(), "{}", text(&out.stderr));
    }

    /// The confidence and the (collection, body) of each result of a query.
    fn query(&self, question: &str) -> (String, Vec<(String, String)>) {
        let out = self.fds(&["--json", "query", question]);
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let rows = json["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["collection"].as_str().unwrap().to_string(),
                    r["body"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        (json["confidence"].as_str().unwrap().to_string(), rows)
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

fn pair(collection: &str, body: &str) -> (String, String) {
    (collection.to_string(), body.to_string())
}

#[test]
fn a_negated_question_is_not_an_exact_match() {
    let sandbox = Sandbox::new();
    sandbox.fact("Can users delete records?", "Yes", "policy");

    for negated in [
        "Cannot users delete records?",
        "Users can not delete records",
        "Can't users delete records?",
    ] {
        let (confidence, _) = sandbox.query(negated);
        assert_ne!(confidence, "exact", "{negated}");
    }

    let (confidence, rows) = sandbox.query("can users delete records");
    assert_eq!(confidence, "exact");
    assert_eq!(rows, [pair("policy", "Yes")]);
}

#[test]
fn the_same_question_in_two_collections_keeps_both_answers() {
    let sandbox = Sandbox::new();
    sandbox.fact("What is the port?", "443", "alpha");
    sandbox.fact("What is the port?", "8080", "beta");

    let listing = text(&sandbox.fds(&["ls"]).stdout);
    assert_eq!(listing.lines().filter(|l| l.contains("port")).count(), 2);

    let out = sandbox.fds(&["--json", "query", "What is the port?", "-c", "alpha"]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 1);
    assert_eq!(json["results"][0]["body"], "443");
}

#[test]
fn a_query_without_a_collection_returns_every_exact_answer() {
    let sandbox = Sandbox::new();
    sandbox.fact("What is the port?", "443", "alpha");
    sandbox.fact("What is the port?", "8080", "beta");

    let (confidence, mut rows) = sandbox.query("What is the port?");
    rows.sort();
    assert_eq!(confidence, "exact");
    assert_eq!(rows, [pair("alpha", "443"), pair("beta", "8080")]);
}

#[test]
fn the_same_question_in_the_same_collection_is_replaced_with_a_warning() {
    let sandbox = Sandbox::new();
    sandbox.fact("What is the port?", "443", "alpha");
    let out = sandbox.fds(&["fact", "add", "what is the PORT", "8080", "-c", "alpha"]);

    assert!(out.status.success());
    assert!(text(&out.stderr).contains("conflicts with an existing record"));
    let (_, rows) = sandbox.query("What is the port?");
    assert_eq!(rows, [pair("alpha", "8080")]);
}
