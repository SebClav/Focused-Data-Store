//! `fds` -- the reference command-line tool for FDS databases (SPEC.md §7).

mod audit;
mod db;
mod etl;
mod guard;
mod ingest;
mod journal;
mod media;
mod mcp;
mod ollama;
mod ops;
mod policy;
mod query;
mod sensitive;
mod sectors;
mod serve;
mod status;
mod tabular;
mod text;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use fds::v4::{DbMeta, Kind, Record};
use serde_json::{json, Value};

use crate::ops::{add_procedures, import_facts, record_json, upsert_fact};

use crate::query::{Confidence, Engine, DEFAULT_BUDGET};
use crate::text::{now, today};

#[derive(Parser)]
#[command(name = "fds", version, about = "FDS (Focused Model Data Store): a database for knowledge that AI systems consume")]
struct Cli {
    /// Database file (default: the single *.fds file in this directory)
    #[arg(long, global = true, value_name = "FILE")]
    db: Option<PathBuf>,

    /// Print machine-readable JSON instead of text
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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

#[derive(Subcommand)]
enum SectorCmd {
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
enum FactCmd {
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
enum ContextCmd {
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
struct JournalSrc {
    /// Read every sector of this manifest instead of one database
    #[arg(long, value_name = "FILE")]
    sectors: Option<PathBuf>,
    /// Only entries made on this branch
    #[arg(long)]
    branch: Option<String>,
    /// Only entries written by this agent
    #[arg(long)]
    agent: Option<String>,
    /// Only entries with this role
    #[arg(long)]
    role: Option<String>,
    /// Only entries from this day on (YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    since: Option<String>,
    /// At most this many, newest first
    #[arg(long)]
    limit: Option<usize>,
    /// For --timeline: also the guard's audit file (with --sectors, the one next to the manifest is used)
    #[arg(long, value_name = "FILE")]
    audit: Option<PathBuf>,
}

#[derive(Subcommand)]
enum JournalCmd {
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
enum AuditCmd {
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
enum PolicyCmd {
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
enum ProcCmd {
    /// Add or replace procedures from a JSON file (one object or an array)
    Add {
        file: PathBuf,
        #[arg(short, long, default_value = "default")]
        collection: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("fds: {e}");
            ExitCode::from(1)
        }
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

async fn run(cli: Cli) -> Result<u8, String> {
    let json_out = cli.json;

    if let Command::Init { name } = &cli.command {
        let path = cli.db.clone().unwrap_or_else(|| PathBuf::from(format!("{name}.fds")));
        if path.exists() {
            return Err(format!("{} already exists.", path.display()));
        }
        let date = today();
        let mut meta = DbMeta { name: name.clone(), created: date.clone(), updated: date, ..Default::default() };
        db::save(&path, &mut meta, &[])?;
        eprintln!("Created empty FDS database {}", path.display());
        return Ok(0);
    }

    // Commands that work on a manifest or a server, not on one local file.
    match &cli.command {
        Command::Sector(cmd) => return sector_cmd(cmd, json_out),
        Command::Journal(cmd) => return journal_cmd(cmd, cli.db.as_deref(), json_out),
        Command::Guard { policies, agent, audit, command } => {
            let agent = agent.clone().or_else(|| std::env::var("FDS_AGENT").ok()).filter(|a| !a.trim().is_empty()).unwrap_or_else(|| "unknown".into());
            let audit = audit.clone().unwrap_or_else(|| audit::beside(policies));
            return guard::run(guard::Config { manifest: policies.clone(), agent, audit, command: command.clone() }).await;
        }
        Command::Audit(cmd) => return audit_cmd(cmd, json_out),
        Command::Check { subject, action, value, sectors, default } => {
            return check_cmd(subject, action, value.as_deref(), sectors.as_deref(), default.as_deref(), cli.db.as_deref(), json_out);
        }
        Command::Serve { port, open, sectors: Some(manifest), .. } => {
            serve::run_sectors(manifest.clone(), *port, *open).await?;
            return Ok(0);
        }
        Command::Mcp { connect: Some(url), .. } => {
            mcp::connect(url).await?;
            return Ok(0);
        }
        Command::Mcp { sectors: Some(manifest), agent, .. } => {
            let m = sectors::Manifest::load(manifest)?;
            let name = agent.as_deref().ok_or("--sectors needs --as <sector>: which agent this is")?;
            let def = m.get(name).ok_or(format!("no sector `{name}` in {}", manifest.display()))?;
            if def.key_sha256.is_none() {
                return Err(format!("`{name}` is not an agent sector; make one with `fds sector new <name> --agent`"));
            }
            let files = m.view_of(def).into_iter().filter_map(|n| m.get(&n).map(|d| (n, m.path_of(d)))).collect();
            let memory = mcp::Memory { path: m.path_of(def), sector: name.to_string(), writer: format!("model:{name}") };
            mcp::run(files, Some(memory), def.clearance.clone()).await?;
            return Ok(0);
        }
        _ => {}
    }

    let path = db::locate(cli.db.as_deref())?;

    match cli.command {
        Command::Init { .. } => unreachable!(),

        Command::Status => {
            let (meta, records) = db::load(&path)?;
            let s = status::summary(&path, &meta, &records);
            if json_out { print_json(&s) } else { status::print_summary(&s) }
        }

        Command::Add { paths, collection, vision_model, lang } => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let db_abs = path.canonicalize().ok();
            let mut report = ingest::Report::default();
            let mut images: Vec<etl::Document> = Vec::new();
            let mut described = 0usize;
            for file in ingest::collect(&paths) {
                if file.canonicalize().ok() == db_abs {
                    continue; // never ingest the database into itself
                }
                let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                if let Some(media_type) = media::media_type(&ext) {
                    use base64::Engine as _;
                    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
                    images.push(etl::Document {
                        name: ingest::source_name(&file),
                        text: String::new(),
                        collection: Some(collection.clone()),
                        data: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
                        media_type: Some(media_type.to_string()),
                    });
                    continue;
                }
                match ingest::fragments(&file) {
                    Some(frags) if !frags.is_empty() => {
                        let source = ingest::source_name(&file);
                        report.fragments += ingest::upsert(&mut records, &source, &collection, frags);
                        report.files += 1;
                    }
                    _ => report.skipped += 1,
                }
            }
            if !images.is_empty() {
                let model = vision_model.clone().unwrap_or_else(media::vision_model);
                eprintln!("{} image(s): new or changed ones are described with `{model}`, one model call each…", images.len());
                let req = etl::ImportRequest {
                    documents: images,
                    collection: Some(collection.clone()),
                    vision_model: Some(model.clone()),
                    lang,
                    on_error: etl::OnError::Skip,
                    ..Default::default()
                };
                let plan = etl::run_async(req, &mut records, |bytes, prompt| {
                    let model = model.clone();
                    async move { ollama::describe_image(&model, &bytes, &prompt).await }
                })
                .await;
                for d in &plan.documents {
                    match d.status {
                        etl::Status::Error => eprintln!("  ✗ {}: {}", d.name, d.message.as_deref().unwrap_or("error")),
                        etl::Status::Unchanged => eprintln!("  = {} (unchanged, not described again)", d.name),
                        _ => {
                            described += 1;
                            eprintln!("  ✓ {}", d.name)
                        }
                    }
                }
            }
            db::save(&path, &mut meta, &records)?;
            eprintln!(
                "Added {} fragments from {} files and {described} image description(s) ({} skipped) to collection `{collection}`.",
                report.fragments, report.files, report.skipped
            );
            warn_missing_embeddings(&meta, &records);
        }

        Command::Import { file, collection, kind, dry_run, skip_errors } => {
            let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let (text, encoding) = tabular::decode(&bytes);
            let rows = match ext.as_str() {
                "csv" | "tsv" | "txt" => {
                    let delim = if ext == "tsv" { '\t' } else { tabular::detect_delimiter(&text) };
                    let table = tabular::parse_csv(&text, delim);
                    if table.len() < 2 {
                        return Err("the table needs a header row and at least one data row".into());
                    }
                    eprintln!(
                        "{}: {} rows, {} columns, separated by {}, {encoding}",
                        file.display(),
                        table.len() - 1,
                        table[0].len(),
                        if delim == '\t' { "tabs".to_string() } else { format!("`{delim}`") }
                    );
                    tabular::rows_from_table(&table[0], &table[1..], kind.as_deref())
                }
                "json" => tabular::rows_from_json(&text)?,
                "jsonl" | "ndjson" => tabular::rows_from_jsonl(&text)?,
                _ => {
                    return Err(format!(
                        "`{}` is not a table. Import reads CSV, TSV, JSON or JSON Lines; use `fds add` for documents and images.",
                        file.display()
                    ))
                }
            };
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let req = etl::ImportRequest {
                dry_run,
                on_error: if skip_errors { etl::OnError::Skip } else { etl::OnError::Abort },
                collection: Some(collection),
                source: Some(ingest::source_name(&file)),
                rows,
                ..Default::default()
            };
            let plan = etl::run_async(req, &mut records, |_bytes, _prompt| async { Err::<String, _>("no images in a table".into()) }).await;
            if plan.written {
                db::save(&path, &mut meta, &records)?;
            }
            if json_out {
                print_json(&serde_json::to_value(&plan).unwrap_or_default());
            } else {
                status::print_plan(&plan);
            }
            if plan.blocked() {
                return Err(format!(
                    "nothing written: {} row(s) have errors. Fix them, or pass --skip-errors to write the rest.",
                    plan.summary.error
                ));
            }
        }

        Command::Fact(FactCmd::Add { question, answer, collection, source }) => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let id = upsert_fact(&mut records, &question, &answer, &collection, &source, "human", &today())?;
            db::save(&path, &mut meta, &records)?;
            if json_out { print_json(&json!({"id": id})) } else { println!("{id}") }
            warn_missing_embeddings(&meta, &records);
        }

        Command::Fact(FactCmd::Import { file, collection }) => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let n = import_facts(&file, &mut records, &collection)?;
            db::save(&path, &mut meta, &records)?;
            eprintln!("Imported {n} facts into collection `{collection}`.");
            warn_missing_embeddings(&meta, &records);
        }

        Command::Proc(ProcCmd::Add { file, collection }) => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let ids = add_procedures(&file, &mut records, &collection)?;
            db::save(&path, &mut meta, &records)?;
            if json_out { print_json(&json!({"ids": ids})) } else { ids.iter().for_each(|id| println!("{id}")) }
            warn_missing_embeddings(&meta, &records);
        }

        Command::Ls { kind, rtype, collection } => {
            let (_, records) = db::load(&path)?;
            let kind = match kind.as_deref() {
                Some(k) => Some(Kind::parse(k).ok_or(format!("unknown kind `{k}` (fact, procedure, knowledge)"))?),
                None => None,
            };
            let rows: Vec<&Record> = records
                .iter()
                .filter(|r| kind.map_or(true, |k| r.kind == k))
                .filter(|r| rtype.as_deref().map_or(true, |t| r.rtype == t))
                .filter(|r| collection.as_deref().map_or(true, |c| r.collection == c))
                .collect();
            if json_out {
                print_json(&Value::Array(rows.iter().map(|r| record_json(r)).collect()));
            } else {
                rows.iter().for_each(|r| status::print_row(r));
            }
        }

        Command::Show { id } => {
            let (_, records) = db::load(&path)?;
            let i = db::resolve_id(&records, &id)?;
            if json_out { print_json(&record_json(&records[i])) } else { status::print_record(&records[i]) }
        }

        Command::Rm { id, source } => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let before = records.len();
            match (id, source) {
                (Some(id), None) => {
                    let i = db::resolve_id(&records, &id)?;
                    records.remove(i);
                }
                (None, Some(src)) => {
                    // Sources are stored relative to where they were added, with
                    // forward slashes; accept the path however it is typed.
                    let stored = ingest::source_name(std::path::Path::new(&src));
                    let slashed = src.replace('\\', "/");
                    records.retain(|r| r.source != src && r.source != stored && r.source != slashed);
                    if records.len() == before {
                        return Err(format!(
                            "no record has source `{src}`. Sources are stored like `{stored}`; `fds ls` shows them."
                        ));
                    }
                }
                _ => return Err("give either an id or --source <path>, not both.".into()),
            }
            let removed = before - records.len();
            db::save(&path, &mut meta, &records)?;
            eprintln!("Removed {removed} record(s).");
        }

        Command::Embed { model, all } => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            eprintln!("Embedding with `{}`…", model.as_deref().unwrap_or(if meta.embedding_model.is_empty() { ops::DEFAULT_EMBED_MODEL } else { &meta.embedding_model }));
            let n = ops::embed(&mut meta, &mut records, model, all).await?;
            if n == 0 {
                eprintln!("All records already have embeddings.");
                return Ok(0);
            }
            db::save(&path, &mut meta, &records)?;
            eprintln!("Done. {} records now have embeddings.", records.iter().filter(|r| r.embedding.is_some()).count());
        }

        Command::Query { text, collection, budget, links, verified_by } => {
            let q = text.join(" ");
            let (meta, records) = db::load(&path)?;
            let engine = Engine::new(meta, records);
            let emb = if engine.has_embeddings() {
                let e = ollama::embed_query(&engine.meta.embedding_model, &q).await;
                if e.is_none() {
                    eprintln!("fds: embeddings unavailable (is Ollama running?); using full-text only.");
                }
                e
            } else {
                None
            };
            let verified = query::verified_list(verified_by.as_deref());
            let scope = query::Scope { collection: collection.as_deref(), verified_by: &verified };
            let mut result = engine.query_scoped(&q, &scope, budget, emb.as_deref());
            if links {
                engine.expand_links(&mut result, &scope);
            }
            if json_out {
                print_json(&serde_json::to_value(&result).unwrap_or_default());
            } else {
                status::print_result(&result);
            }
            if result.confidence == Confidence::None {
                return Ok(2);
            }
        }

        Command::Context(ContextCmd::Ls) => {
            let (meta, records) = db::load(&path)?;
            let engine = Engine::new(meta, records);
            let list: Vec<_> = engine.contexts().into_iter().map(|i| engine.context(i)).collect();
            if json_out {
                print_json(&json!(list
                    .iter()
                    .map(|b| json!({"id": b.id, "name": b.name, "collection": b.collection, "records": b.records.len(),
                                    "missing": b.missing.len(), "tokens": b.tokens, "hash": b.hash}))
                    .collect::<Vec<_>>()));
            } else if list.is_empty() {
                println!("(no contexts)");
            } else {
                for b in &list {
                    println!("{}  {:<28} {:>3} records  {:>5} tokens  {}", &b.id[..8], b.name, b.records.len(), b.tokens, b.hash);
                }
            }
        }

        Command::Context(ContextCmd::Show { name, known }) => {
            let (meta, records) = db::load(&path)?;
            let engine = Engine::new(meta, records);
            let i = engine.find_context(&name).ok_or(format!("no context named `{name}`; `fds context ls` lists them"))?;
            let b = engine.context(i);
            if known.as_deref() == Some(b.hash.as_str()) {
                if json_out {
                    print_json(&json!({"name": b.name, "hash": b.hash, "tokens": b.tokens, "changed": false}));
                } else {
                    println!("unchanged  {}  {}", b.name, b.hash);
                }
            } else if json_out {
                let mut v = serde_json::to_value(&b).unwrap_or_default();
                v["changed"] = json!(true);
                print_json(&v);
            } else {
                status::print_context(&b);
            }
        }

        Command::Context(ContextCmd::Set { name, instructions, file, links, collection }) => {
            let text = match (instructions, file) {
                (Some(t), _) => t,
                (None, Some(f)) => std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?,
                (None, None) => String::new(),
            };
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let r = ops::context_record(&name, &text, &links, &collection, "cli", "human", &today())?;
            let id = ops::upsert(&mut records, r);
            db::save(&path, &mut meta, &records)?;
            if json_out { print_json(&json!({"id": id})) } else { println!("{id}") }
        }

        Command::Context(ContextCmd::Import { file, name, collection }) => {
            let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let name = name.unwrap_or_else(|| file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "instructions".into()));
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            // The file's own `[[links]]` become the context's members, as with `context set`.
            let r = ops::context_record(&name, &text, &[], &collection, &ingest::source_name(&file), "human", &today())?;
            let id = ops::upsert(&mut records, r);
            db::save(&path, &mut meta, &records)?;
            if json_out {
                print_json(&json!({"id": id, "name": name}));
            } else {
                eprintln!("Context `{name}` loaded from {}; agents get it with fds_context, and pay for it once.", file.display());
            }
        }

        Command::Context(ContextCmd::Export { name }) => {
            let (meta, records) = db::load(&path)?;
            let engine = Engine::new(meta, records);
            let i = engine.find_context(&name).ok_or(format!("no context named `{name}`; `fds context ls` lists them"))?;
            let b = engine.context(i);
            // Instructions as written, then each linked record as a section:
            // plain Markdown any coding agent reads as AGENTS.md.
            let mut out = b.instructions.trim_end().to_string();
            out.push('\n');
            if !b.records.is_empty() {
                out.push_str("\n## Reference\n");
                for h in &b.records {
                    out.push_str(&format!("\n### {}\n\n{}\n", h.question.as_deref().unwrap_or(&h.title), h.body.trim_end()));
                }
            }
            print!("{out}");
        }

        Command::Serve { port, open, clearance, .. } => serve::run(path, port, open, clearance).await?,
        Command::Mcp { write, agent, clearance, .. } => {
            let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "db".into());
            let memory = write.then(|| mcp::Memory {
                path: path.clone(),
                sector: name.clone(),
                writer: format!("model:{}", agent.as_deref().unwrap_or("agent")),
            });
            mcp::run(vec![(name, path)], memory, clearance).await?
        }
        Command::Sector(_) | Command::Journal(_) | Command::Check { .. } | Command::Guard { .. } | Command::Audit(_) => unreachable!(),

        Command::Policy(PolicyCmd::Add { text, subject, action, rule, min, max, values, unit, tool, tool_subject, tool_action, tool_value }) => {
            let mut attrs = std::collections::BTreeMap::new();
            attrs.insert("subject".to_string(), subject.trim().to_string());
            attrs.insert("action".to_string(), action.trim().to_string());
            attrs.insert("rule".to_string(), rule.trim().to_string());
            match rule.trim() {
                "deny" => {}
                "range" => {
                    let ok = |v: &Option<String>| v.as_deref().and_then(policy::number).is_some();
                    if !ok(&min) || !ok(&max) {
                        return Err("a range policy needs numeric --min and --max".into());
                    }
                    if policy::number(min.as_deref().unwrap()).unwrap() > policy::number(max.as_deref().unwrap()).unwrap() {
                        return Err("--min is above --max: nothing could ever pass".into());
                    }
                }
                "one_of" => {
                    if values.as_deref().map_or(true, |v| v.split(',').all(|x| x.trim().is_empty())) {
                        return Err("a one_of policy needs --values a,b,c".into());
                    }
                }
                other => return Err(format!("unknown rule `{other}`: use deny, range or one_of")),
            }
            for (k, v) in [("min", &min), ("max", &max), ("values", &values), ("unit", &unit)] {
                if let Some(v) = v.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                    attrs.insert(k.to_string(), v.to_string());
                }
            }
            if let Some(t) = tool.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                let map = json!({
                    "tool": t,
                    "subject": tool_subject.clone().unwrap_or_else(|| subject.clone()),
                    "action": tool_action.clone().unwrap_or_else(|| action.clone()),
                    "value": tool_value,
                });
                attrs.insert("guard".to_string(), map.to_string());
            }
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let id = upsert_fact(&mut records, &text, &text, "policy", "owner", "human", &today())?;
            let r = records.iter_mut().find(|r| r.id == id).ok_or("policy was not stored")?;
            r.rtype = policy::TYPE.to_string();
            r.attrs = attrs;
            db::save(&path, &mut meta, &records)?;
            if json_out { print_json(&json!({"id": id})) } else { println!("{id}") }
        }

        Command::Policy(PolicyCmd::Ls) => {
            let (_, records) = db::load(&path)?;
            let counted: Vec<String> = policy::policies(&records).into_iter().map(|p| p.id).collect();
            let rows: Vec<&Record> = records.iter().filter(|r| r.rtype == policy::TYPE).collect();
            if json_out {
                let v: Vec<Value> = rows
                    .iter()
                    .map(|r| json!({"id": r.id, "text": r.body, "attrs": r.attrs, "verified_by": r.verified_by, "counts": counted.contains(&r.id)}))
                    .collect();
                print_json(&Value::Array(v));
            } else if rows.is_empty() {
                println!("no policies");
            } else {
                for r in rows {
                    let a = |k: &str| r.attrs.get(k).cloned().unwrap_or_default();
                    let counts = if counted.contains(&r.id) { String::new() } else { format!("  [IGNORED: verified by {}, not by a person]", r.verified_by) };
                    println!("{}  {} {} {}{}  {}{counts}", &r.id[..r.id.len().min(8)], a("rule"), a("subject"), a("action"), policy_bounds(r), r.body);
                }
            }
        }

        Command::Log { summary, why, files, branch, commit, by, role, status, to } => {
            let (git_branch, git_commit) = if branch.is_none() || commit.is_none() { journal::git_context() } else { Default::default() };
            let (branch, commit) = (branch.unwrap_or(git_branch), commit.unwrap_or(git_commit));
            let at = now();
            let agent = by.as_deref().unwrap_or("");
            let draft = journal::Draft {
                summary: &summary,
                why: why.as_deref().unwrap_or(""),
                files: &files,
                branch: &branch,
                commit: &commit,
                agent,
                at: &at,
                role: role.as_deref().unwrap_or(""),
                status: status.as_deref().unwrap_or(""),
                to: to.as_deref().unwrap_or(""),
            };
            let verified_by = if agent.is_empty() { "human".to_string() } else { format!("model:{agent}") };
            let id = journal::append(&path, journal::record(&draft, &verified_by)?)?;
            if json_out { print_json(&json!({"id": id})) } else { println!("{id}") }
        }
    }
    Ok(0)
}

/// The entries a `journal` command works on, newest first, and a name for the page.
fn journal_entries(src: &JournalSrc, db_file: Option<&Path>) -> Result<(Vec<journal::Entry>, String), String> {
    let filter = journal::Filter { branch: src.branch.clone(), agent: src.agent.clone(), role: src.role.clone(), since: src.since.clone(), limit: None };
    let mut all = Vec::new();
    let name;
    if let Some(manifest) = &src.sectors {
        let m = sectors::Manifest::load(manifest)?;
        name = "all sectors".to_string();
        for def in &m.sectors {
            let (_, records) = db::load(&m.path_of(def))?;
            all.extend(journal::entries(&records, &def.name, &filter));
        }
    } else {
        let path = db::locate(db_file)?;
        let (meta, records) = db::load(&path)?;
        name = if meta.name.is_empty() { path.display().to_string() } else { meta.name };
        all = journal::entries(&records, "", &filter);
    }
    journal::sort_newest_first(&mut all);
    if let Some(n) = src.limit {
        all.truncate(n);
    }
    Ok((all, name))
}

fn policy_bounds(r: &Record) -> String {
    let a = |k: &str| r.attrs.get(k).cloned().unwrap_or_default();
    match a("rule").as_str() {
        "range" => format!(" [{}..{}{}{}]", a("min"), a("max"), if a("unit").is_empty() { "" } else { " " }, a("unit")),
        "one_of" => format!(" [{}]", a("values")),
        _ => String::new(),
    }
}

/// The policies that count, and what to answer when none matches: from one
/// database, or from every sector of a manifest (a policy counts wherever a
/// person verified it, and never otherwise).
fn policy_source(db_file: Option<&Path>, manifest: Option<&Path>) -> Result<(Vec<policy::Policy>, policy::Fallback), String> {
    if let Some(mpath) = manifest {
        let m = sectors::Manifest::load(mpath)?;
        let mut all = Vec::new();
        for def in &m.sectors {
            let (_, records) = db::load(&m.path_of(def))?;
            all.extend(policy::policies(&records));
        }
        let fallback = m.default.as_deref().map(policy::Fallback::parse).transpose()?.unwrap_or_default();
        return Ok((all, fallback));
    }
    let path = db::locate(db_file)?;
    let (_, records) = db::load(&path)?;
    Ok((policy::policies(&records), policy::Fallback::default()))
}

fn check_cmd(subject: &str, action: &str, value: Option<&str>, manifest: Option<&Path>, default: Option<&str>, db_file: Option<&Path>, json_out: bool) -> Result<u8, String> {
    let (policies, mut fallback) = policy_source(db_file, manifest)?;
    if let Some(d) = default {
        fallback = policy::Fallback::parse(d)?;
    }
    let decision = policy::check(&policies, subject, action, value, fallback);
    if json_out {
        print_json(&serde_json::to_value(&decision).map_err(|e| e.to_string())?);
    } else {
        println!("{}", decision.summary());
    }
    Ok(if decision.allowed() { 0 } else { policy::DENY_EXIT })
}

/// The black box: journal entries and, when there is an audit file, the guard's
/// decisions, in the order they happened.
fn journal_steps(src: &JournalSrc, entries: &[journal::Entry]) -> Result<Vec<journal::Step>, String> {
    let mut steps: Vec<journal::Step> = entries.iter().map(journal::Step::of).collect();
    let audit_file = src.audit.clone().or_else(|| src.sectors.as_deref().map(audit::beside).filter(|p| p.exists()));
    if let Some(f) = audit_file {
        steps.extend(audit::steps(&f)?);
    }
    journal::sort_oldest_first(&mut steps);
    Ok(steps)
}

fn audit_cmd(cmd: &AuditCmd, json_out: bool) -> Result<u8, String> {
    let default = || PathBuf::from(audit::FILE);
    match cmd {
        AuditCmd::Show { file, limit } => {
            let path = file.clone().unwrap_or_else(default);
            let mut steps = audit::steps(&path)?;
            steps.reverse();
            if let Some(n) = limit {
                steps.truncate(*n);
            }
            if json_out {
                print_json(&serde_json::to_value(&steps).map_err(|e| e.to_string())?);
            } else if steps.is_empty() {
                println!("nothing recorded in {}", path.display());
            } else {
                for st in steps {
                    println!("{}  #{}  {}", st.at, st.order, st.text);
                }
            }
            Ok(0)
        }
        AuditCmd::Verify { file } => {
            let path = file.clone().unwrap_or_else(default);
            let r = audit::verify(&path)?;
            if json_out {
                print_json(&json!({"entries": r.lines, "intact": r.broken.is_none(), "head": r.head, "broken_at": r.broken.as_ref().map(|b| b.0), "why": r.broken.as_ref().map(|b| b.1.clone())}));
            } else if let Some((line, why)) = &r.broken {
                println!("BROKEN at line {line}: {why}");
            } else {
                println!("{} entries, the chain is intact. Last hash: {}", r.lines, r.head);
                println!("Keep that hash where the agent cannot write: cut-off lines at the end only show against it.");
            }
            Ok(if r.broken.is_none() { 0 } else { 1 })
        }
    }
}

fn journal_cmd(cmd: &JournalCmd, db_file: Option<&Path>, json_out: bool) -> Result<u8, String> {
    match cmd {
        JournalCmd::Show { src, md, timeline } => {
            let (entries, name) = journal_entries(src, db_file)?;
            if *timeline {
                let steps = journal_steps(src, &entries)?;
                if json_out {
                    print_json(&serde_json::to_value(&steps).map_err(|e| e.to_string())?);
                } else if *md {
                    print!("{}", journal::timeline_markdown(&steps, &name));
                } else {
                    print!("{}", journal::timeline_text(&steps));
                }
            } else if json_out {
                print_json(&serde_json::to_value(&entries).map_err(|e| e.to_string())?);
            } else if *md {
                print!("{}", journal::markdown(&entries, &name));
            } else {
                print!("{}", journal::text(&entries));
            }
        }
        JournalCmd::Export { src, out } => {
            let (entries, name) = journal_entries(src, db_file)?;
            std::fs::write(out, journal::markdown(&entries, &name)).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
            eprintln!("Wrote {} to {}.", journal::summary_line(&entries), out.display());
        }
        JournalCmd::Publish { src, repo, to } => {
            let (entries, name) = journal_entries(src, db_file)?;
            let md = journal::markdown(&entries, &name);
            let js = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())? + "\n";
            let message = format!("journal: {}", journal::summary_line(&entries));
            let steps = journal_steps(src, &entries)?;
            let tl = journal::timeline_markdown(&steps, &name);
            let files: [(&str, &[u8]); 3] = [("JOURNAL.md", md.as_bytes()), ("TIMELINE.md", tl.as_bytes()), ("journal.json", js.as_bytes())];
            let done = journal::publish(repo, to, &files, &message)?;
            let short = &done.commit[..done.commit.len().min(10)];
            if done.unchanged {
                eprintln!("Branch `{to}` already has this journal ({short}); nothing to commit.");
            } else {
                eprintln!("{} branch `{to}` at {short} with {}.", if done.created { "Created" } else { "Updated" }, journal::summary_line(&entries));
                eprintln!("Your files, your index and your current branch were not touched. To share it: git push origin {to}");
            }
            if json_out {
                print_json(&json!({"branch": to, "commit": done.commit, "unchanged": done.unchanged}));
            }
        }
    }
    Ok(0)
}

fn sector_cmd(cmd: &SectorCmd, json_out: bool) -> Result<u8, String> {
    match cmd {
        SectorCmd::New { name, agent, reads, file, manifest, clearance } => {
            let mut m = if manifest.exists() { sectors::Manifest::load(manifest)? } else { sectors::Manifest::new(manifest) };
            if m.get(name).is_some() {
                return Err(format!("sector `{name}` already exists in {}", manifest.display()));
            }
            let file = file.clone().unwrap_or_else(|| format!("{name}.fds"));
            let key = agent.then(sectors::new_key);
            m.sectors.push(sectors::SectorDef {
                name: name.clone(),
                file: file.clone(),
                reads: reads.clone(),
                key_sha256: key.as_deref().map(sectors::key_hash),
                clearance: clearance.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect(),
            });
            let path = m.path_of(m.get(name).unwrap());
            let created = !path.exists();
            if created {
                let date = today();
                let mut meta = DbMeta { name: name.clone(), created: date.clone(), updated: date, ..Default::default() };
                // Checked before anything is written: a bad name or a
                // missing `reads` target leaves no stray database behind.
                m.save(manifest)?;
                db::save(&path, &mut meta, &[])?;
            } else {
                m.save(manifest)?;
            }
            if json_out {
                print_json(&json!({"name": name, "file": file, "created": created, "key": key}));
            } else {
                eprintln!("Sector `{name}` → {}{}", path.display(), if created { " (new, empty)" } else { " (existing file)" });
                if let Some(k) = key {
                    eprintln!("Key for agent `{name}`, shown only now. Give it to the agent as FDS_KEY or as `Authorization: Bearer`:");
                    println!("{k}");
                }
            }
        }
        SectorCmd::Ls { manifest } => {
            let m = sectors::Manifest::load(manifest)?;
            let mut rows = Vec::new();
            for d in &m.sectors {
                let records = db::load(&m.path_of(d)).map(|(_, r)| r.len()).ok();
                rows.push(json!({"name": d.name, "file": d.file, "agent": d.key_sha256.is_some(), "reads": d.reads, "records": records}));
            }
            if json_out {
                print_json(&Value::Array(rows));
            } else {
                for r in &rows {
                    let reads: Vec<String> = r["reads"].as_array().unwrap().iter().filter_map(|x| x.as_str().map(String::from)).collect();
                    println!(
                        "{:<16} {:<6} {:>7} records  {:<20} reads: {}",
                        r["name"].as_str().unwrap_or(""),
                        if r["agent"].as_bool() == Some(true) { "agent" } else { "shared" },
                        r["records"].as_u64().map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                        r["file"].as_str().unwrap_or(""),
                        if reads.is_empty() { "—".to_string() } else { reads.join(", ") }
                    );
                }
            }
        }
        SectorCmd::Key { name, manifest } => {
            let mut m = sectors::Manifest::load(manifest)?;
            let key = sectors::new_key();
            let def = m.get_mut(name).ok_or(format!("no sector `{name}` in {}", manifest.display()))?;
            def.key_sha256 = Some(sectors::key_hash(&key));
            m.save(manifest)?;
            if json_out {
                print_json(&json!({"name": name, "key": key}));
            } else {
                eprintln!("New key for agent `{name}`, shown only now; the old key no longer works:");
                println!("{key}");
            }
        }
        SectorCmd::Promote { id, from, to, manifest } => {
            let m = sectors::Manifest::load(manifest)?;
            let src = m.path_of(m.get(from).ok_or(format!("no sector `{from}`"))?);
            let dst = m.path_of(m.get(to).ok_or(format!("no sector `{to}`"))?);
            if src == dst {
                return Err("--from and --to are the same sector".into());
            }
            // Both locks, always in the same order: two promotions running
            // the opposite way can never hold one each and wait forever.
            let (first, second) = if src < dst { (&src, &dst) } else { (&dst, &src) };
            let _l1 = db::lock(first)?;
            let _l2 = db::lock(second)?;
            let (mut src_meta, mut src_records) = db::load(&src)?;
            let (mut dst_meta, mut dst_records) = db::load(&dst)?;
            let i = db::resolve_id(&src_records, id)?;
            let mut r = src_records[i].clone();
            let was = std::mem::replace(&mut r.verified_by, "human".into());
            r.last_verified = today();
            let new_id = ops::upsert(&mut dst_records, r);
            // The copy is saved before the original goes: a crash between
            // the two leaves the record twice, never nowhere.
            db::save(&dst, &mut dst_meta, &dst_records)?;
            src_records.remove(i);
            db::save(&src, &mut src_meta, &src_records)?;
            if json_out {
                print_json(&json!({"id": new_id, "from": from, "to": to, "was_verified_by": was}));
            } else {
                eprintln!("Promoted {} from `{from}` to `{to}`; it was {was}, it is now verified by a person.", &new_id[..new_id.len().min(12)]);
            }
        }
    }
    Ok(0)
}

fn warn_missing_embeddings(meta: &DbMeta, records: &[Record]) {
    if meta.embedding_model.is_empty() {
        return;
    }
    let missing = records.iter().filter(|r| r.embedding.is_none()).count();
    if missing > 0 {
        eprintln!("note: {missing} record(s) have no embedding yet; run `fds embed`.");
    }
}
