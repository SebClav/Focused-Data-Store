//! One handler per CLI command, each acting on a loaded [`Store`].

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fds::v4::Kind;
use serde_json::{json, Value};

use crate::cli::{Command, ContextCommand, FactCommand, ProcCommand};
use crate::db::Store;
use crate::output::{print_value, public_record};
use crate::query::query_records;
use crate::record::{context_id, fact, record, same_identity};

/// Run one command and return the process exit code.
pub fn dispatch(store: &mut Store, command: Command, json: bool) -> Result<u8, String> {
    match command {
        Command::Status => status(store, json),
        Command::Fact {
            command: FactCommand::Add { question, answer, collection, source },
        } => fact_add(store, &question, &answer, &collection, &source)?,
        Command::Fact {
            command: FactCommand::Import { file, collection },
        } => fact_import(store, &file, &collection)?,
        Command::Proc {
            command: ProcCommand::Add { file, collection },
        } => proc_add(store, &file, &collection)?,
        Command::Ls { kind, r#type, collection } => {
            ls(store, kind.as_deref(), r#type.as_deref(), collection.as_deref(), json)
        }
        Command::Show { id } => show(store, &id, json)?,
        Command::Rm { id, source } => rm(store, id.as_deref(), source.as_deref())?,
        Command::Query { text, collection, budget, links, verified_by } => {
            return Ok(query(
                store,
                &text.join(" "),
                collection.as_deref(),
                budget,
                links,
                verified_by.as_deref(),
                json,
            ));
        }
        Command::Context {
            command: ContextCommand::Set { name, instructions, file, links, collection },
        } => context_set(store, &name, instructions, file, links, &collection)?,
        Command::Context { command: ContextCommand::Ls } => context_ls(store, json),
        Command::Context {
            command: ContextCommand::Show { name, known },
        } => context_show(store, &name, known.as_deref(), json)?,
        Command::Context {
            command: ContextCommand::Import { file, name, collection },
        } => context_import(store, &file, name, &collection)?,
        Command::Context {
            command: ContextCommand::Export { name },
        } => context_export(store, &name)?,
        Command::Add { paths, collection, .. } => add(store, &paths, &collection)?,
        Command::Import { file, collection, dry_run, .. } => {
            import(store, &file, &collection, dry_run)?
        }
        Command::Embed { .. }
        | Command::Mcp { .. }
        | Command::Sector { .. }
        | Command::Log { .. }
        | Command::Journal { .. }
        | Command::Policy { .. }
        | Command::Check { .. }
        | Command::Guard { .. }
        | Command::Audit { .. } => {
            return Err(
                "this command group is part of the FDS interface and is not implemented in this initial clean build yet"
                    .into(),
            )
        }
        Command::Init { .. } => unreachable!(),
    }
    Ok(0)
}

fn status(store: &Store, json: bool) {
    let records = &store.records;
    let mut collections = BTreeMap::<String, usize>::new();
    for r in records {
        *collections.entry(r.collection.clone()).or_default() += 1;
    }
    let count = |kind: Kind| records.iter().filter(|r| r.kind == kind).count();
    let bytes = fs::metadata(&store.path).map(|m| m.len()).unwrap_or(0);
    let value = json!({
        "database": store.path,
        "bytes": bytes,
        "records": records.len(),
        "facts": count(Kind::Fact),
        "procedures": count(Kind::Procedure),
        "knowledge": count(Kind::Knowledge),
        "embedded": records.iter().filter(|r| r.embedding.is_some()).count(),
        "collections": collections,
    });
    print_value(value, json);
}

fn fact_add(
    store: &mut Store,
    question: &str,
    answer: &str,
    collection: &str,
    source: &str,
) -> Result<(), String> {
    let r = fact(question, answer, collection, source)?;
    if let Some(existing) = fact_conflict(&store.records, &r) {
        eprintln!(
            "warning: fact {} conflicts with an existing record in collection `{}` (answer or source differs); the new fact will replace it",
            existing.id, existing.collection
        );
    }
    let id = store.upsert(r);
    store.save()?;
    println!("{id}");
    Ok(())
}

/// The fact that saving `incoming` would replace with a different answer or
/// source. The same question in another collection is a different fact.
fn fact_conflict<'a>(
    records: &'a [fds::v4::Record],
    incoming: &fds::v4::Record,
) -> Option<&'a fds::v4::Record> {
    records.iter().find(|existing| {
        existing.kind == Kind::Fact
            && same_identity(existing, incoming)
            && (existing.body != incoming.body || existing.source != incoming.source)
    })
}

fn fact_import(store: &mut Store, file: &Path, collection: &str) -> Result<(), String> {
    let v = read_json(file)?;
    let rows = v
        .as_array()
        .or_else(|| v.get("entries").and_then(Value::as_array))
        .ok_or("expected an array of facts")?;
    let mut n = 0;
    for row in rows {
        let q = row
            .get("question")
            .or_else(|| row.get("canonical_query"))
            .and_then(Value::as_str)
            .ok_or("fact is missing question")?;
        let a = row
            .get("answer")
            .and_then(Value::as_str)
            .ok_or("fact is missing answer")?;
        let source = row
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("import");
        store.upsert(fact(q, a, collection, source)?);
        n += 1;
    }
    store.save()?;
    println!("{n} facts imported");
    Ok(())
}

fn proc_add(store: &mut Store, file: &Path, collection: &str) -> Result<(), String> {
    let v = read_json(file)?;
    let rows = v.as_array().cloned().unwrap_or_else(|| vec![v]);
    // Validate every row before touching the database.
    let mut staged = Vec::new();
    for row in rows {
        let intent = row
            .get("intent")
            .and_then(Value::as_str)
            .ok_or("procedure is missing intent")?;
        let steps = row.get("steps").ok_or("procedure is missing steps")?;
        if !steps.is_array() {
            return Err("procedure steps must be an array".into());
        }
        let body = json!({"steps": steps});
        staged.push(record(
            Kind::Procedure,
            collection,
            Some(intent),
            body.to_string(),
            intent,
            &file.display().to_string(),
            "human",
            "",
        ));
    }
    for r in staged {
        store.upsert(r);
    }
    store.save()?;
    println!("procedures added");
    Ok(())
}

fn ls(
    store: &Store,
    kind: Option<&str>,
    rtype: Option<&str>,
    collection: Option<&str>,
    json: bool,
) {
    let out: Vec<_> = store
        .records
        .iter()
        .filter(|r| kind.is_none_or(|x| x == r.kind.as_str()))
        .filter(|r| rtype.is_none_or(|x| x == r.rtype))
        .filter(|r| collection.is_none_or(|x| x == r.collection))
        .map(public_record)
        .collect();
    print_value(json!(out), json);
}

fn show(store: &Store, id: &str, json: bool) -> Result<(), String> {
    let r = store
        .records
        .iter()
        .find(|r| r.id == id || r.id.starts_with(id))
        .ok_or_else(|| format!("record not found: {id}"))?;
    print_value(public_record(r), json);
    Ok(())
}

fn rm(store: &mut Store, id: Option<&str>, source: Option<&str>) -> Result<(), String> {
    let before = store.records.len();
    store.records.retain(|r| match source {
        Some(s) => r.source != s,
        None => Some(r.id.as_str()) != id,
    });
    let removed = before - store.records.len();
    if removed == 0 {
        return Err("no matching record".into());
    }
    store.save()?;
    println!("removed {removed} record(s)");
    Ok(())
}

/// Prints the result and returns exit code 2 when nothing matched.
fn query(
    store: &Store,
    text: &str,
    collection: Option<&str>,
    budget: u32,
    links: bool,
    verified_by: Option<&str>,
    json: bool,
) -> u8 {
    let result = query_records(&store.records, text, collection, budget, links, verified_by);
    let found = result["confidence"] != "none";
    print_value(result, json);
    if found {
        0
    } else {
        2
    }
}

fn context_set(
    store: &mut Store,
    name: &str,
    instructions: Option<String>,
    file: Option<PathBuf>,
    links: Vec<String>,
    collection: &str,
) -> Result<(), String> {
    let text = match (instructions, file) {
        (Some(s), None) => s,
        (None, Some(p)) => fs::read_to_string(p).map_err(|e| e.to_string())?,
        _ => return Err("provide exactly one of --instructions or --file".into()),
    };
    let mut r = record(
        Kind::Knowledge,
        collection,
        None,
        text,
        name,
        "manual",
        "human",
        "context",
    );
    r.id = context_id(name);
    r.links = links;
    store.upsert(r);
    store.save()?;
    println!("context saved: {name}");
    Ok(())
}

fn context_ls(store: &Store, json: bool) {
    let out: Vec<_> = store
        .records
        .iter()
        .filter(|r| r.rtype == "context")
        .map(public_record)
        .collect();
    print_value(json!(out), json);
}

fn find_context<'a>(store: &'a Store, name: &str) -> Result<&'a fds::v4::Record, String> {
    store
        .records
        .iter()
        .find(|r| r.rtype == "context" && r.title.eq_ignore_ascii_case(name))
        .ok_or_else(|| "context not found".into())
}

fn context_show(store: &Store, name: &str, known: Option<&str>, json: bool) -> Result<(), String> {
    let r = find_context(store, name)?;
    if known == Some(r.id.as_str()) {
        println!("unchanged");
    } else {
        print_value(public_record(r), json);
    }
    Ok(())
}

fn context_import(
    store: &mut Store,
    file: &Path,
    name: Option<String>,
    collection: &str,
) -> Result<(), String> {
    let text = fs::read_to_string(file).map_err(|e| e.to_string())?;
    let title = name
        .or_else(|| {
            file.file_stem()
                .and_then(|x| x.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "context".into());
    let mut r = record(
        Kind::Knowledge,
        collection,
        None,
        text,
        &title,
        &file.display().to_string(),
        "human",
        "context",
    );
    r.id = context_id(&title);
    store.upsert(r);
    store.save()?;
    println!("context imported: {title}");
    Ok(())
}

fn context_export(store: &Store, name: &str) -> Result<(), String> {
    println!("{}", find_context(store, name)?.body);
    Ok(())
}

fn add(store: &mut Store, paths: &[PathBuf], collection: &str) -> Result<(), String> {
    let mut n = 0;
    for p in paths {
        if p.is_dir() {
            for entry in fs::read_dir(p).map_err(|e| e.to_string())? {
                let f = entry.map_err(|e| e.to_string())?.path();
                if f.is_file() {
                    n += ingest_text(store, &f, collection)?;
                }
            }
        } else {
            n += ingest_text(store, p, collection)?;
        }
    }
    store.save()?;
    println!("{n} knowledge records added");
    Ok(())
}

/// Read a UTF-8 text file and add it as one knowledge record.
fn ingest_text(store: &mut Store, path: &Path, collection: &str) -> Result<usize, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let title = path
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("document");
    let r = record(
        Kind::Knowledge,
        collection,
        None,
        text,
        title,
        &path.display().to_string(),
        "ingest",
        "",
    );
    store.upsert(r);
    Ok(1)
}

fn import(store: &mut Store, file: &Path, collection: &str, dry_run: bool) -> Result<(), String> {
    let v = read_json(file)?;
    let rows = v.as_array().cloned().unwrap_or_else(|| vec![v]);
    if dry_run {
        println!("{} rows inspected", rows.len());
        return Ok(());
    }
    for row in rows {
        let text = |key: &str| row.get(key).and_then(Value::as_str);
        if let (Some(q), Some(a)) = (text("question"), text("answer")) {
            store.upsert(fact(q, a, collection, "import")?);
        }
    }
    store.save()?;
    println!("import complete");
    Ok(())
}

/// Deserialize a JSON file and include its path in any parse error.
fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::fact_conflict;
    use crate::record::record;
    use fds::v4::{Kind, Record};

    fn fact(answer: &str, collection: &str, source: &str) -> Record {
        record(
            Kind::Fact,
            collection,
            Some("What is the service port?"),
            answer.into(),
            "Service port",
            source,
            "human",
            "",
        )
    }

    #[test]
    fn detects_a_different_answer_for_the_same_fact() {
        let existing = fact("443", "network", "spec");
        let proposed = fact("8443", "network", "spec");
        assert!(fact_conflict(&[existing], &proposed).is_some());
    }

    #[test]
    fn does_not_warn_when_fact_content_and_provenance_match() {
        let existing = fact("443", "network", "spec");
        let repeated = fact("443", "network", "spec");
        assert!(fact_conflict(&[existing], &repeated).is_none());
    }

    #[test]
    fn detects_when_upsert_would_replace_a_fact_source() {
        let existing = fact("443", "network", "spec");
        let moved = fact("443", "network", "runtime-config");
        assert!(fact_conflict(&[existing], &moved).is_some());
    }

    #[test]
    fn the_same_question_in_another_collection_is_not_a_conflict() {
        let existing = fact("443", "network", "spec");
        let other = fact("8443", "operations", "runtime-config");
        assert!(fact_conflict(&[existing], &other).is_none());
    }
}
