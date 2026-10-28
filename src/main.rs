//! Command-line interface for the FDS local knowledge database.
//!
//! The CLI reads and writes FDS v4 files, exposes records as JSON or terminal
//! output, and keeps database selection explicit when multiple files exist.
//! Graphical and network-serving features are intentionally excluded from
//! this CLI-only package.

mod cli;
mod commands;
mod db;
mod output;
mod query;
mod record;
mod text;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};
use db::Store;

/// Parse arguments, run the requested command, and return a process exit code.
fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("fds: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Execute one CLI command against the selected database.
///
/// Mutating commands hold the writer lock from database read through save so
/// concurrent CLI processes cannot silently overwrite each other's updates.
fn run(cli: Cli) -> Result<u8, String> {
    ensure_read_only_allows(cli.read_only, &cli.command)?;
    // Initialization creates a new file and therefore does not need a lookup.
    if let Command::Init { name } = &cli.command {
        return db::init(name);
    }
    let path = db::locate(cli.db.as_deref())?;
    // Keep the lock alive for the full read-modify-write command.
    let _write_guard = if cli.command.is_write() {
        Some(db::lock_database(&path)?)
    } else {
        None
    };
    let mut store = Store::open(path)?;
    commands::dispatch(&mut store, cli.command, cli.json)
}

fn ensure_read_only_allows(read_only: bool, command: &Command) -> Result<(), String> {
    if read_only && (matches!(command, Command::Init { .. }) || command.is_write()) {
        return Err("--read-only prevents commands that create or modify a database".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ensure_read_only_allows;
    use crate::cli::{Command, FactCommand};

    #[test]
    fn read_only_rejects_database_creation_and_mutations() {
        assert!(ensure_read_only_allows(true, &Command::Init { name: "new".into() }).is_err());
        assert!(ensure_read_only_allows(
            true,
            &Command::Fact {
                command: FactCommand::Add {
                    question: "q".into(),
                    answer: "a".into(),
                    collection: "default".into(),
                    source: "manual".into(),
                },
            }
        )
        .is_err());
    }

    #[test]
    fn read_only_allows_queries_and_status() {
        assert!(ensure_read_only_allows(true, &Command::Status).is_ok());
        assert!(ensure_read_only_allows(
            true,
            &Command::Query {
                text: vec!["context".into()],
                collection: None,
                budget: 400,
                links: false,
                verified_by: None,
            }
        )
        .is_ok());
    }
}
