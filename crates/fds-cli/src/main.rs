//! `fds` -- the reference command-line tool for FDS databases (SPEC.md §7).

mod cli;
mod cmd_journal;
mod cmd_sector;
mod db;
mod ingest;
mod journal;
mod mcp;
mod ops;
mod query;
mod review;
mod sectors;
mod sensitive;
mod status;
mod text;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use fds::v4::{DbMeta, Kind, Record};
use serde_json::{json, Value};

use crate::cli::{Cli, Command, ContextCmd, FactCmd, ProcCmd};
use crate::cmd_journal::journal_cmd;
use crate::cmd_sector::sector_cmd;
use crate::ops::{
    add_procedures, fact_record, import_facts, record_json, replaced_fact, replaced_warning,
};

use crate::query::{Confidence, Engine};
use crate::text::{now, today};

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

pub(crate) fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

async fn run(cli: Cli) -> Result<u8, String> {
    let json_out = cli.json;

    if cli.read_only && cli.command.writes() {
        return Err(
            "--read-only prevents commands that create or modify a database or a sector".into(),
        );
    }

    if let Command::Init { name } = &cli.command {
        let path = cli
            .db
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("{name}.fds")));
        if path.exists() {
            return Err(format!("{} already exists.", path.display()));
        }
        let date = today();
        let mut meta = DbMeta {
            name: name.clone(),
            created: date.clone(),
            updated: date,
            ..Default::default()
        };
        db::save(&path, &mut meta, &[])?;
        eprintln!("Created empty FDS database {}", path.display());
        return Ok(0);
    }

    // Commands that work on a manifest or a server, not on one local file.
    match &cli.command {
        Command::Sector(cmd) => return sector_cmd(cmd, json_out),
        Command::Journal(cmd) => return journal_cmd(cmd, cli.db.as_deref(), json_out),
        Command::Review(args) => return review::review_cmd(args, cli.db.as_deref(), json_out),
        Command::Mcp {
            sectors: Some(manifest),
            agent,
            ..
        } => {
            let m = sectors::Manifest::load(manifest)?;
            let name = agent
                .as_deref()
                .ok_or("--sectors needs --as <sector>: which agent this is")?;
            let def = m
                .get(name)
                .ok_or(format!("no sector `{name}` in {}", manifest.display()))?;
            if def.key_sha256.is_none() {
                return Err(format!("`{name}` is not an agent sector; make one with `fds sector new <name> --agent`"));
            }
            let files = m
                .view_of(def)
                .into_iter()
                .filter_map(|n| m.get(&n).map(|d| (n, m.path_of(d))))
                .collect();
            let memory = (!cli.read_only).then(|| mcp::Memory {
                path: m.path_of(def),
                sector: name.to_string(),
                writer: format!("model:{name}"),
            });
            mcp::run(files, memory, def.clearance.clone()).await?;
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
            if json_out {
                print_json(&s)
            } else {
                status::print_summary(&s)
            }
        }

        Command::Add { paths, collection } => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let db_abs = path.canonicalize().ok();
            let mut report = ingest::Report::default();
            for file in ingest::collect(&paths) {
                if file.canonicalize().ok() == db_abs {
                    continue;
                }
                match ingest::fragments(&file) {
                    Some(frags) if !frags.is_empty() => {
                        report.fragments += ingest::upsert(
                            &mut records,
                            &ingest::source_name(&file),
                            &collection,
                            frags,
                        );
                        report.files += 1;
                    }
                    _ => report.skipped += 1,
                }
            }
            db::save(&path, &mut meta, &records)?;
            eprintln!(
                "{} files, {} fragments, {} skipped",
                report.files, report.fragments, report.skipped
            );
        }

        Command::Import { file, collection } => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let imported = import_facts(&file, &mut records, &collection)?;
            db::save(&path, &mut meta, &records)?;
            for warning in &imported.warnings {
                eprintln!("{warning}");
            }
            eprintln!("Imported {} facts.", imported.count);
        }

        Command::Fact(FactCmd::Add {
            question,
            answer,
            collection,
            source,
        }) => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let fact = fact_record(&question, &answer, &collection, &source, "human", &today())?;
            if let Some(existing) = replaced_fact(&records, &fact) {
                eprintln!("{}", replaced_warning(existing, &fact));
            }
            let id = ops::upsert(&mut records, fact);
            db::save(&path, &mut meta, &records)?;
            if json_out {
                print_json(&json!({"id": id}))
            } else {
                println!("{id}")
            }
        }

        Command::Fact(FactCmd::Import { file, collection }) => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let imported = import_facts(&file, &mut records, &collection)?;
            db::save(&path, &mut meta, &records)?;
            for warning in &imported.warnings {
                eprintln!("{warning}");
            }
            eprintln!(
                "Imported {} facts into collection `{collection}`.",
                imported.count
            );
        }

        Command::Proc(ProcCmd::Add { file, collection }) => {
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let ids = add_procedures(&file, &mut records, &collection)?;
            db::save(&path, &mut meta, &records)?;
            if json_out {
                print_json(&json!({"ids": ids}))
            } else {
                ids.iter().for_each(|id| println!("{id}"))
            }
        }

        Command::Ls {
            kind,
            rtype,
            collection,
        } => {
            let (_, records) = db::load(&path)?;
            let kind = match kind.as_deref() {
                Some(k) => Some(
                    Kind::parse(k)
                        .ok_or(format!("unknown kind `{k}` (fact, procedure, knowledge)"))?,
                ),
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
            if json_out {
                print_json(&record_json(&records[i]))
            } else {
                status::print_record(&records[i])
            }
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
                    records
                        .retain(|r| r.source != src && r.source != stored && r.source != slashed);
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

        Command::Query {
            text,
            collection,
            budget,
            links,
            verified_by,
        } => {
            let q = text.join(" ");
            let (meta, records) = db::load(&path)?;
            let engine = Engine::new(meta, records);
            let emb: Option<Vec<f32>> = None;
            let verified = query::verified_list(verified_by.as_deref());
            let scope = query::Scope {
                collection: collection.as_deref(),
                verified_by: &verified,
            };
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
            let list: Vec<_> = engine
                .contexts()
                .into_iter()
                .map(|i| engine.context(i))
                .collect();
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
                    println!(
                        "{}  {:<28} {:>3} records  {:>5} tokens  {}",
                        &b.id[..8],
                        b.name,
                        b.records.len(),
                        b.tokens,
                        b.hash
                    );
                }
            }
        }

        Command::Context(ContextCmd::Show { name, known }) => {
            let (meta, records) = db::load(&path)?;
            let engine = Engine::new(meta, records);
            let i = engine.find_context(&name).ok_or(format!(
                "no context named `{name}`; `fds context ls` lists them"
            ))?;
            let b = engine.context(i);
            if known.as_deref() == Some(b.hash.as_str()) {
                if json_out {
                    print_json(
                        &json!({"name": b.name, "hash": b.hash, "tokens": b.tokens, "changed": false}),
                    );
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

        Command::Context(ContextCmd::Set {
            name,
            instructions,
            file,
            links,
            collection,
        }) => {
            let text = match (instructions, file) {
                (Some(t), _) => t,
                (None, Some(f)) => {
                    std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?
                }
                (None, None) => String::new(),
            };
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            let r =
                ops::context_record(&name, &text, &links, &collection, "cli", "human", &today())?;
            let id = ops::upsert(&mut records, r);
            db::save(&path, &mut meta, &records)?;
            if json_out {
                print_json(&json!({"id": id}))
            } else {
                println!("{id}")
            }
        }

        Command::Context(ContextCmd::Import {
            file,
            name,
            collection,
        }) => {
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let name = name.unwrap_or_else(|| {
                file.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "instructions".into())
            });
            let _lock = db::lock(&path)?;
            let (mut meta, mut records) = db::load(&path)?;
            // The file's own `[[links]]` become the context's members, as with `context set`.
            let r = ops::context_record(
                &name,
                &text,
                &[],
                &collection,
                &ingest::source_name(&file),
                "human",
                &today(),
            )?;
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
            let i = engine.find_context(&name).ok_or(format!(
                "no context named `{name}`; `fds context ls` lists them"
            ))?;
            let b = engine.context(i);
            // Instructions as written, then each linked record as a section:
            // plain Markdown any coding agent reads as AGENTS.md.
            let mut out = b.instructions.trim_end().to_string();
            out.push('\n');
            if !b.records.is_empty() {
                out.push_str("\n## Reference\n");
                for h in &b.records {
                    out.push_str(&format!(
                        "\n### {}\n\n{}\n",
                        h.question.as_deref().unwrap_or(&h.title),
                        h.body.trim_end()
                    ));
                }
            }
            print!("{out}");
        }

        Command::Mcp {
            write,
            agent,
            clearance,
            ..
        } => {
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "db".into());
            let memory = write.then(|| mcp::Memory {
                path: path.clone(),
                sector: name.clone(),
                writer: format!("model:{}", agent.as_deref().unwrap_or("agent")),
            });
            mcp::run(vec![(name, path)], memory, clearance).await?
        }
        Command::Sector(_) | Command::Journal(_) | Command::Review(_) => unreachable!(),

        Command::Log {
            summary,
            why,
            files,
            branch,
            commit,
            by,
            role,
            status,
            to,
        } => {
            let (git_branch, git_commit) = if branch.is_none() || commit.is_none() {
                journal::git_context()
            } else {
                Default::default()
            };
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
            let verified_by = if agent.is_empty() {
                "human".to_string()
            } else {
                format!("model:{agent}")
            };
            let id = journal::append(&path, journal::record(&draft, &verified_by)?)?;
            if json_out {
                print_json(&json!({"id": id}))
            } else {
                println!("{id}")
            }
        }
    }
    Ok(0)
}
