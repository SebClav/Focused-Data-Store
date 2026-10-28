//! Command-line arguments.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Parsed options shared by all FDS commands.
#[derive(Parser)]
#[command(
    name = "fds",
    version,
    about = "FDS — a database for knowledge that AI systems consume"
)]
pub struct Cli {
    /// Database file (defaults to the only *.fds file in the current directory)
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,
    /// Emit machine-readable JSON
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

/// Commands exposed by the FDS CLI.
///
/// Core database operations are implemented in this package. Some advanced
/// FDS commands remain reserved and return a clear unsupported-command error.
#[derive(Subcommand)]
pub enum Command {
    Init {
        #[arg(default_value = "knowledge")]
        name: String,
    },
    Status,
    Add {
        paths: Vec<PathBuf>,
        #[arg(short, long, default_value = "default")]
        collection: String,
        #[arg(long)]
        lang: Option<String>,
    },
    Fact {
        #[command(subcommand)]
        command: FactCommand,
    },
    Proc {
        #[command(subcommand)]
        command: ProcCommand,
    },
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    Ls {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long = "type")]
        r#type: Option<String>,
        #[arg(short, long)]
        collection: Option<String>,
    },
    Show {
        id: String,
    },
    Rm {
        id: Option<String>,
        #[arg(long)]
        source: Option<String>,
    },
    Query {
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(short, long)]
        collection: Option<String>,
        #[arg(long, default_value_t = 400)]
        budget: u32,
        #[arg(long)]
        links: bool,
        #[arg(long)]
        verified_by: Option<String>,
    },
    Import {
        file: PathBuf,
        #[arg(short, long, default_value = "default")]
        collection: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        skip_errors: bool,
    },
    Embed {
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        all: bool,
    },
    Mcp {
        #[arg(long)]
        write: bool,
        #[arg(long = "as")]
        agent: Option<String>,
        #[arg(long)]
        connect: Option<String>,
    },
    Sector {
        #[command(subcommand)]
        command: SectorCommand,
    },
    Log {
        summary: String,
        #[arg(long)]
        why: Option<String>,
        #[arg(long = "file")]
        files: Vec<String>,
        #[arg(long)]
        by: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        to: Option<String>,
    },
    Journal {
        #[command(subcommand)]
        command: JournalCommand,
    },
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    Check {
        #[arg(long)]
        subject: String,
        #[arg(long)]
        action: String,
        #[arg(long, allow_hyphen_values = true)]
        value: Option<String>,
    },
    Guard {
        #[arg(long)]
        policies: PathBuf,
        #[arg(long)]
        agent: Option<String>,
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    Audit {
        #[command(subcommand)]
        command: AuditCommand,
    },
}

impl Command {
    /// Whether the command rewrites the database file.
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Command::Add { .. }
                | Command::Fact { .. }
                | Command::Proc { .. }
                | Command::Rm { .. }
                | Command::Import { .. }
                | Command::Context {
                    command: ContextCommand::Set { .. } | ContextCommand::Import { .. }
                }
        )
    }
}

/// Operations for creating and importing question-and-answer facts.
#[derive(Subcommand)]
pub enum FactCommand {
    Add {
        question: String,
        answer: String,
        #[arg(short, long, default_value = "default")]
        collection: String,
        #[arg(long, default_value = "manual")]
        source: String,
    },
    Import {
        file: PathBuf,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
}

/// Operations for adding procedural records from JSON.
#[derive(Subcommand)]
pub enum ProcCommand {
    Add {
        file: PathBuf,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
}

/// Operations for named, longer-form context records.
#[derive(Subcommand)]
pub enum ContextCommand {
    Set {
        name: String,
        #[arg(long)]
        instructions: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long = "link")]
        links: Vec<String>,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
    Ls,
    Show {
        name: String,
        #[arg(long)]
        known: Option<String>,
    },
    Import {
        file: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
    Export {
        name: String,
    },
}

/// Operations reserved for the future sector-access interface.
#[derive(Subcommand)]
pub enum SectorCommand {
    New {
        name: String,
        #[arg(long)]
        agent: bool,
        #[arg(long)]
        reads: Vec<String>,
        #[arg(long)]
        clearance: Vec<String>,
    },
    Ls,
    Key {
        name: String,
    },
    Promote {
        id: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
}

/// Operations reserved for journal export and publishing.
#[derive(Subcommand)]
pub enum JournalCommand {
    Show {
        #[arg(long)]
        timeline: bool,
        #[arg(long)]
        md: bool,
    },
    Export {
        #[arg(long)]
        out: PathBuf,
    },
    Publish {
        #[arg(long)]
        repo: PathBuf,
    },
}

/// Operations reserved for structured policy rules.
#[derive(Subcommand)]
pub enum PolicyCommand {
    Add {
        description: String,
        #[arg(long)]
        subject: String,
        #[arg(long)]
        action: String,
        #[arg(long)]
        rule: String,
        #[arg(long)]
        min: Option<f64>,
        #[arg(long)]
        max: Option<f64>,
        #[arg(long)]
        unit: Option<String>,
    },
    Ls,
}

/// Operations reserved for audit-log inspection and verification.
#[derive(Subcommand)]
pub enum AuditCommand {
    Show,
    Verify,
}
