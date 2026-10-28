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
