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
    /// the audit file. For an agent that runs this tool; it is not a
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
        /// Vision model that describes images (default: FDS_VISION_MODEL, else qwen2.5vl:3b)
        #[arg(long, value_name = "MODEL")]
        vision_model: Option<String>,
        /// Language to describe images in: es, en, fr, de, ...
        #[arg(long, value_name = "CODE")]
        lang: Option<String>,
    },
    /// Import rows from CSV, TSV, JSON or JSON Lines (facts, procedures, contexts)
    Import {
        file: PathBuf,
        #[arg(short, long, default_value = "default")]
        collection: String,
        /// Kind of every row when the file has no kind column: fact, procedure or context
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
        /// Show what would happen, and write nothing
        #[arg(long)]
        dry_run: bool,
        /// Write the valid rows and report the rest, instead of all-or-nothing
        #[arg(long)]
        skip_errors: bool,
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
    /// Ask whether an action breaks a policy: allow (exit 0) or deny (exit 3), with no model
    Check {
        /// What the action touches, e.g. fridge.chamber3.setpoint
        #[arg(long)]
        subject: String,
        /// The operation: set, delete, drop, read_raw...
        #[arg(long)]
        action: String,
        /// The value being set, when the rule is a range or a list
        #[arg(long, allow_hyphen_values = true)]
        value: Option<String>,
        /// Read the policies of every sector of this manifest
        #[arg(long, value_name = "FILE")]
        sectors: Option<PathBuf>,
        /// What to answer when no policy matches: allow or deny (default: the manifest's, else allow)
        #[arg(long)]
        default: Option<String>,
    },
    /// Manage policies: rules a person verified, enforced by `check` and the guard
    #[command(subcommand)]
    Policy(PolicyCmd),
    /// Stand between an agent and the real tool server: check every tool call
    /// against the policies, forward what passes, refuse what does not, and
    /// record each decision. The agent's host starts this instead of the server.
    Guard {
        /// The sectors manifest whose policies apply (and whose `default` decides the rest)
        #[arg(long, value_name = "FILE")]
        policies: PathBuf,
        /// Who the agent is, for the audit (default: $FDS_AGENT, else `unknown`)
        #[arg(long)]
        agent: Option<String>,
        /// Where decisions are recorded (default: audit.jsonl next to the manifest)
        #[arg(long, value_name = "FILE")]
        audit: Option<PathBuf>,
        /// The real MCP server's command, after `--`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Read or verify the guard's audit file
    #[command(subcommand)]
    Audit(AuditCmd),
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
    /// Compute and store embeddings (needs Ollama)
    Embed {
        #[arg(long)]
        model: Option<String>,
        /// Recompute every record, not only those missing one
        #[arg(long)]
        all: bool,
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
    /// Serve the query API, the database manager and MCP over HTTP
    Serve {
        #[arg(long, default_value_t = 7070)]
        port: u16,
        /// Open the manager in the default browser
        #[arg(long)]
        open: bool,
        /// Serve every sector in this manifest instead of one database
        #[arg(long, value_name = "MANIFEST")]
        sectors: Option<PathBuf>,
        /// Sensitivity levels `/mcp` may show in full, comma-separated (one database; with sectors each agent's manifest entry decides)
        #[arg(long, value_delimiter = ',')]
        clearance: Vec<String>,
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
        #[arg(long, value_name = "MANIFEST", conflicts_with = "connect")]
        sectors: Option<PathBuf>,
        /// Forward to a running `fds serve` instead of loading files (key in FDS_KEY)
        #[arg(long, value_name = "URL")]
        connect: Option<String>,
        /// Sensitivity levels the agent may read in full, comma-separated (one database; with sectors its manifest entry decides)
        #[arg(long, value_delimiter = ',')]
        clearance: Vec<String>,
    },
    /// Sectors: one database per agent, plus shared ones it may read
    #[command(subcommand)]
    Sector(SectorCmd),
}

impl Command {
    /// Whether the command creates or modifies a database, a sector or the
    /// audit file, and so is refused under `--read-only`.
    ///
    /// `journal export` only writes a Markdown file and is allowed. `serve`
    /// is refused because its manager and `/mcp` can write; `mcp` is refused
    /// only with `--write`.
    pub fn writes(&self) -> bool {
        match self {
            Command::Init { .. }
            | Command::Add { .. }
            | Command::Import { .. }
            | Command::Fact(_)
            | Command::Proc(_)
            | Command::Log { .. }
            | Command::Guard { .. }
            | Command::Rm { .. }
            | Command::Embed { .. }
            | Command::Serve { .. } => true,
            Command::Context(cmd) => {
                matches!(cmd, ContextCmd::Set { .. } | ContextCmd::Import { .. })
            }
            Command::Journal(cmd) => matches!(cmd, JournalCmd::Publish { .. }),
            Command::Policy(cmd) => matches!(cmd, PolicyCmd::Add { .. }),
            Command::Sector(cmd) => !matches!(cmd, SectorCmd::Ls { .. }),
            Command::Mcp { write, .. } => *write,
            Command::Status
            | Command::Check { .. }
            | Command::Audit(_)
            | Command::Ls { .. }
            | Command::Show { .. }
            | Command::Query { .. } => false,
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
    /// For --timeline: also the guard's audit file (with --sectors, the one next to the manifest is used)
    #[arg(long, value_name = "FILE")]
    pub audit: Option<PathBuf>,
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
}

#[derive(Subcommand)]
pub enum AuditCmd {
    /// List the decisions, newest first
    Show {
        /// The audit file (default: audit.jsonl in this directory)
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Check that no line was changed, removed or inserted; prints the hash of the last line
    Verify {
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum PolicyCmd {
    /// Add or replace a policy
    Add {
        /// The rule in your own words; it is what a denial shows
        text: String,
        /// What it protects; a trailing * matches a prefix (db.prod.*)
        #[arg(long)]
        subject: String,
        /// The operation it covers, or * for every one
        #[arg(long, default_value = "*")]
        action: String,
        /// deny, range or one_of
        #[arg(long)]
        rule: String,
        /// Lower bound, inclusive (rule range)
        #[arg(long, allow_hyphen_values = true)]
        min: Option<String>,
        /// Upper bound, inclusive (rule range)
        #[arg(long, allow_hyphen_values = true)]
        max: Option<String>,
        /// Allowed values, comma-separated (rule one_of)
        #[arg(long)]
        values: Option<String>,
        /// Shown in answers: C, rpm, USD...
        #[arg(long)]
        unit: Option<String>,
        /// For the guard: the tool call this policy governs
        #[arg(long)]
        tool: Option<String>,
        /// For the guard: the subject of that call, with {argument} placeholders (default: --subject)
        #[arg(long)]
        tool_subject: Option<String>,
        /// For the guard: the action of that call (default: --action)
        #[arg(long)]
        tool_action: Option<String>,
        /// For the guard: the value of that call, e.g. {celsius}
        #[arg(long)]
        tool_value: Option<String>,
    },
    /// List the policies, and say which ones do not count
    Ls,
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
            &["import", "rows.csv"],
            &["fact", "add", "q", "a"],
            &["fact", "import", "facts.json"],
            &["proc", "add", "procs.json"],
            &["context", "set", "n", "--instructions", "t"],
            &["context", "import", "AGENTS.md"],
            &["log", "did a thing"],
            &["journal", "publish"],
            &[
                "policy",
                "add",
                "text",
                "--subject",
                "s",
                "--action",
                "a",
                "--rule",
                "deny",
            ],
            &["guard", "--policies", "sectors.json", "--", "server"],
            &["rm", "id"],
            &["embed"],
            &["serve"],
            &["mcp", "--write"],
            &["sector", "new", "sales"],
            &["sector", "key", "sales"],
            &["sector", "promote", "id", "--from", "a", "--to", "b"],
        ] {
            assert!(writes(args), "{args:?} should be a write");
        }
    }

    #[test]
    fn commands_that_only_read_are_not_writes() {
        for args in [
            &["status"][..],
            &["check", "--subject", "s", "--action", "a"],
            &["audit", "show"],
            &["audit", "verify"],
            &["ls"],
            &["show", "id"],
            &["query", "some", "words"],
            &["context", "ls"],
            &["context", "show", "n"],
            &["context", "export", "n"],
            &["journal", "show"],
            &["journal", "export"],
            &["policy", "ls"],
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
