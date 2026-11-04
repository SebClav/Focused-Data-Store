//! Command-line arguments (SPEC.md §7).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::query::DEFAULT_BUDGET;
use crate::{journal, sectors};

#[derive(Parser)]
#[command(
    name = "fds",
    version,
    about = "FDS (Focused Model Data Store): a database for knowledge that AI systems consume"
)]
pub struct Cli {
    /// Database file (default: the single *.fds file in this directory)
    #[arg(long, global = true, value_name = "FILE")]
    pub db: Option<PathBuf>,

    /// Print machine-readable JSON instead of text
    #[arg(long, global = true)]
    pub json: bool,

    /// Refuse every command that creates or modifies a database, a sector or
    /// manifest. For an agent that runs this tool; it is not a
    /// substitute for file permissions.
    #[arg(long, global = true)]
    pub read_only: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create an empty database
    Init {
        /// Database name; creates <name>.fds
        #[arg(default_value = "knowledge")]
        name: String,
    },
    /// Show counts by kind and collection, embeddings, size
    Status,
    /// Ingest files or folders as knowledge
    Add {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
    /// Import facts from a JSON object or array
    Import {
        file: PathBuf,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
    /// Manage verified facts
    #[command(subcommand)]
    Fact(FactCmd),
    /// Manage procedures
    #[command(subcommand)]
    Proc(ProcCmd),
    /// Manage contexts: named instructions plus the records they link to
    #[command(subcommand)]
    Context(ContextCmd),
    /// Write down what was done, for the developers who read the code later
    Log {
        /// One line: what changed
        summary: String,
        /// Why it changed, and what a reader needs to know
        #[arg(long)]
        why: Option<String>,
        /// A file that changed (repeat for several)
        #[arg(long = "file", value_name = "PATH")]
        files: Vec<String>,
        /// Default: the branch checked out in this directory
        #[arg(long)]
        branch: Option<String>,
        /// Default: the commit checked out in this directory
        #[arg(long)]
        commit: Option<String>,
        /// Log it as written by this agent; a person reviews it later
        #[arg(long, value_name = "AGENT")]
        by: Option<String>,
        /// The part this step plays: me, reasoning, decision, execution...
        #[arg(long)]
        role: Option<String>,
        /// Where it stands: running, finished, blocked...
        #[arg(long)]
        status: Option<String>,
        /// Who it was handed to, when it was a delegation
        #[arg(long)]
        to: Option<String>,
    },
    /// Read, export or publish the change journal
    #[command(subcommand)]
    Journal(JournalCmd),
    /// List the lines agents flagged in the journal and open the editor on them
    Review(ReviewArgs),
    /// List records
    Ls {
        #[arg(long)]
        kind: Option<String>,
        /// Only records of this type (image, command, ...)
        #[arg(long = "type", value_name = "TYPE")]
        rtype: Option<String>,
        #[arg(short, long)]
        collection: Option<String>,
    },
    /// Print one record with its provenance
    Show { id: String },
    /// Remove a record, or every record from a source
    Rm {
        id: Option<String>,
        #[arg(long, value_name = "PATH")]
        source: Option<String>,
    },
    /// Query the database
    Query {
        #[arg(required = true)]
        text: Vec<String>,
        #[arg(short, long)]
        collection: Option<String>,
        #[arg(long, default_value_t = DEFAULT_BUDGET)]
        budget: u32,
        /// Also return what the results link to, one hop out
        #[arg(long)]
        links: bool,
        /// Only records verified by these: human, ingest, model: (any model); comma-separated
        #[arg(long, value_name = "WHO")]
        verified_by: Option<String>,
    },
    /// Serve FDS's tools to an agent over MCP (stdio)
    Mcp {
        /// Let the agent save and remove its own records (fds_remember, fds_forget)
        #[arg(long)]
        write: bool,
        /// Who the agent is: its sector with --sectors, else the name its records carry
        #[arg(long = "as", value_name = "NAME")]
        agent: Option<String>,
        /// Read the agent's sector, and the ones it reads, from this manifest
        #[arg(long, value_name = "MANIFEST")]
        sectors: Option<PathBuf>,
        /// Sensitivity levels the agent may read in full, comma-separated (one database; with sectors its manifest entry decides)
        #[arg(long, value_delimiter = ',')]
        clearance: Vec<String>,
    },
    /// Sectors: one database per agent, plus shared ones it may read
    #[command(subcommand)]
    Sector(SectorCmd),
}

impl Command {
    /// Whether this command changes a database or sector manifest.
    /// Read-only MCP exposes no memory or logging tools.
    pub fn writes(&self) -> bool {
        match self {
            Command::Init { .. }
            | Command::Add { .. }
            | Command::Import { .. }
            | Command::Fact(_)
            | Command::Proc(_)
            | Command::Log { .. }
            | Command::Rm { .. } => true,
            Command::Context(cmd) => {
                matches!(cmd, ContextCmd::Set { .. } | ContextCmd::Import { .. })
            }
            Command::Journal(cmd) => {
                matches!(cmd, JournalCmd::Publish { .. } | JournalCmd::Review { .. })
            }
            Command::Review(_) => false,
            Command::Sector(cmd) => !matches!(cmd, SectorCmd::Ls { .. }),
            Command::Mcp { write, .. } => *write,
            Command::Status | Command::Ls { .. } | Command::Show { .. } | Command::Query { .. } => {
                false
            }
        }
    }
}

#[derive(Subcommand)]
pub enum SectorCmd {
    /// Add a sector to the manifest, creating its database if needed
    New {
        name: String,
        /// An agent lives here: mint its key (shown once)
        #[arg(long)]
        agent: bool,
        /// Sectors it may read, comma-separated, searched in this order
        #[arg(long, value_delimiter = ',', value_name = "SECTORS")]
        reads: Vec<String>,
        /// Database file, relative to the manifest (default: <name>.fds)
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
        #[arg(long, default_value = sectors::MANIFEST, value_name = "MANIFEST")]
        manifest: PathBuf,
        /// Sensitivity levels this sector's agent may read in full (personal, secret...); comma-separated
        #[arg(long, value_delimiter = ',')]
        clearance: Vec<String>,
    },
    /// List the sectors, what each reads and how many records it holds
    Ls {
        #[arg(long, default_value = sectors::MANIFEST, value_name = "MANIFEST")]
        manifest: PathBuf,
    },
    /// Give an agent sector a new key; the old one stops working
    Key {
        name: String,
        #[arg(long, default_value = sectors::MANIFEST, value_name = "MANIFEST")]
        manifest: PathBuf,
    },
    /// Move a record an agent wrote into another sector, verified by a person
    Promote {
        id: String,
        #[arg(long, value_name = "SECTOR")]
        from: String,
        #[arg(long, value_name = "SECTOR")]
        to: String,
        #[arg(long, default_value = sectors::MANIFEST, value_name = "MANIFEST")]
        manifest: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum FactCmd {
    /// Add or replace a verified fact
    Add {
        question: String,
        answer: String,
        #[arg(short, long, default_value = "default")]
        collection: String,
        #[arg(long, default_value = "manual")]
        source: String,
    },
    /// Import facts from a JSON file
    Import {
        file: PathBuf,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
}

#[derive(Subcommand)]
pub enum ContextCmd {
    /// List contexts with their size and hash
    Ls,
    /// Print a context: its instructions and every record it links to
    Show {
        name: String,
        /// The hash you already hold; prints one line if nothing changed
        #[arg(long, value_name = "HASH")]
        known: Option<String>,
    },
    /// Create or replace a context
    Set {
        name: String,
        /// Instructions, inline. `[[links]]` in them count as links.
        #[arg(long, conflicts_with = "file")]
        instructions: Option<String>,
        /// Instructions from a file (Markdown works well)
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// A record to include, by question, intent or title; repeatable
        #[arg(long = "link", value_name = "TARGET")]
        links: Vec<String>,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
    /// Load an instructions file (AGENTS.md, CLAUDE.md, any Markdown) as a context
    Import {
        file: PathBuf,
        /// Context name (default: the file name without extension)
        #[arg(long)]
        name: Option<String>,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
    /// Print a context as Markdown, ready to save as AGENTS.md
    Export { name: String },
}

/// Where the journal is read from, and what to keep.
#[derive(clap::Args, Clone)]
pub struct JournalSrc {
    /// Read every sector of this manifest instead of one database
    #[arg(long, value_name = "FILE")]
    pub sectors: Option<PathBuf>,
    /// Only entries made on this branch
    #[arg(long)]
    pub branch: Option<String>,
    /// Only entries written by this agent
    #[arg(long)]
    pub agent: Option<String>,
    /// Only entries with this role
    #[arg(long)]
    pub role: Option<String>,
    /// Only entries from this day on (YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    /// At most this many, newest first
    #[arg(long)]
    pub limit: Option<usize>,
}

/// `fds review`: where agents asked a person to look, and the editor opened there.
#[derive(clap::Args, Clone)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub src: JournalSrc,
    /// Only list the flagged lines; do not open the editor
    #[arg(long)]
    pub list: bool,
    /// Include entries a person already reviewed
    #[arg(long)]
    pub all: bool,
    /// Only the entry whose id starts with this
    #[arg(long, value_name = "ID")]
    pub entry: Option<String>,
    /// The project folder the flagged paths are relative to (default: this directory)
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,
    /// The editor command (default: $FDS_EDITOR, then `code`)
    #[arg(long, value_name = "CMD")]
    pub editor: Option<String>,
}

#[derive(Subcommand)]
pub enum JournalCmd {
    /// List the entries, newest first
    Show {
        #[command(flatten)]
        src: JournalSrc,
        /// Print Markdown instead of one block per entry
        #[arg(long)]
        md: bool,
        /// The black box: every step as a numbered line, oldest first
        #[arg(long)]
        timeline: bool,
    },
    /// Write the journal to a Markdown file
    Export {
        #[command(flatten)]
        src: JournalSrc,
        #[arg(short, long, default_value = "JOURNAL.md")]
        out: PathBuf,
    },
    /// Commit the journal to a branch of a git repository, without touching
    /// the files, the index or the branch you are working on
    Publish {
        #[command(flatten)]
        src: JournalSrc,
        /// The repository (default: this directory)
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// The branch that receives the journal
        #[arg(long, default_value = journal::DEFAULT_BRANCH)]
        to: String,
    },
    /// Mark an entry as reviewed by a person (what the editor's Approve button runs).
    /// The entry stays where it is; `fds sector promote` is what moves it to another sector
    Review {
        id: String,
        /// The sector it lives in, when the journal is read across sectors
        #[arg(long, value_name = "SECTOR")]
        sector: Option<String>,
        #[arg(long, default_value = sectors::MANIFEST, value_name = "MANIFEST")]
        manifest: PathBuf,
    },
}

#[derive(Subcommand)]
pub enum ProcCmd {
    /// Add or replace procedures from a JSON file (one object or an array)
    Add {
        file: PathBuf,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cli;

    #[test]
    fn basic_distribution_exposes_only_local_agent_workflows() {
        for command in ["serve", "embed", "policy", "check", "guard", "audit"] {
            assert!(Cli::try_parse_from(["fds", command]).is_err(), "{command}");
        }
        assert!(Cli::try_parse_from(["fds", "mcp", "--connect", "http://localhost:7070/mcp"]).is_err());
        assert!(Cli::try_parse_from(["fds", "mcp", "--write", "--as", "backend"]).is_ok());
        assert!(Cli::try_parse_from(["fds", "review", "--list"]).is_ok());
    }

    fn writes(args: &[&str]) -> bool {
        let mut argv = vec!["fds"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
            .unwrap_or_else(|e| panic!("{args:?} does not parse: {e}"))
            .command
            .writes()
    }

    #[test]
    fn every_command_that_changes_state_is_a_write() {
        for args in [
            &["init"][..],
            &["add", "docs"],
            &["import", "facts.json"],
            &["fact", "add", "q", "a"],
            &["fact", "import", "facts.json"],
            &["proc", "add", "procs.json"],
            &["context", "set", "n", "--instructions", "t"],
            &["context", "import", "AGENTS.md"],
            &["log", "did a thing"],
            &["journal", "publish"],
            &["rm", "id"],
            &["mcp", "--write"],
            &["sector", "new", "sales"],
            &["sector", "key", "sales"],
            &["sector", "promote", "id", "--from", "a", "--to", "b"],
            &["journal", "review", "id"],
        ] {
            assert!(writes(args), "{args:?} should be a write");
        }
    }

    #[test]
    fn commands_that_only_read_are_not_writes() {
        for args in [
            &["status"][..],
            &["ls"],
            &["show", "id"],
            &["query", "some", "words"],
            &["context", "ls"],
            &["context", "show", "n"],
            &["context", "export", "n"],
            &["journal", "show"],
            &["journal", "export"],
            &["mcp"],
            &["sector", "ls"],
        ] {
            assert!(!writes(args), "{args:?} should not be a write");
        }
    }

    #[test]
    fn the_flag_is_global_and_off_by_default() {
        assert!(
            Cli::try_parse_from(["fds", "--read-only", "status"])
                .unwrap()
                .read_only
        );
        assert!(
            Cli::try_parse_from(["fds", "status", "--read-only"])
                .unwrap()
                .read_only
        );
        assert!(!Cli::try_parse_from(["fds", "status"]).unwrap().read_only);
    }
}
