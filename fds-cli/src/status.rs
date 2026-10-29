//! Database summaries (`fds status`, GET /status) and human-readable output.

use std::collections::BTreeMap;
use std::path::Path;

use fds::v4::{DbMeta, Kind, Record};
use serde_json::json;

use crate::query::{ContextBundle, Hit, QueryResult};

pub fn summary(path: &Path, meta: &DbMeta, records: &[Record]) -> serde_json::Value {
    let count = |k: Kind| records.iter().filter(|r| r.kind == k).count();
    let mut collections: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records {
        *collections.entry(r.collection.as_str()).or_insert(0) += 1;
    }
    let embedded = records.iter().filter(|r| r.embedding.is_some()).count();
    let is_ctx = |r: &&Record| r.rtype == crate::query::CONTEXT_TYPE;
    let contexts = records.iter().filter(is_ctx).count();
    // Owner-defined types, for the manager's explorer. Contexts have their own count.
    let mut types: BTreeMap<&str, usize> = BTreeMap::new();
    for r in records.iter().filter(|r| !r.rtype.is_empty() && !is_ctx(r)) {
        *types.entry(r.rtype.as_str()).or_insert(0) += 1;
    }
    // Who vouches for what: the manager's review tile counts what a model wrote
    // and no person has checked yet.
    let by = |pred: &dyn Fn(&str) -> bool| records.iter().filter(|r| pred(&r.verified_by)).count();
    let verified = json!({
        "human": by(&|v| v == "human"),
        "ingest": by(&|v| v == "ingest"),
        "model": by(&|v| v.starts_with("model:")),
    });
    let format = fds::v4::format_version(path).unwrap_or(0);
    json!({
        "database": path.display().to_string(),
        "format": format,
        "verified": verified,
        "name": meta.name,
        "bytes": std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        "records": records.len(),
        "facts": count(Kind::Fact),
        "procedures": count(Kind::Procedure),
        "knowledge": count(Kind::Knowledge) - contexts,
        "contexts": contexts,
        "types": types,
        "collections": collections,
        "embedding_model": meta.embedding_model,
        "embedding_dims": meta.embedding_dims,
        "embedded_records": embedded,
        "created": meta.created,
        "updated": meta.updated,
    })
}

fn human_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1_048_576.0)
    }
}

pub fn print_summary(s: &serde_json::Value) {
    let collections = s["collections"]
        .as_object()
        .map(|m| m.iter().map(|(k, v)| format!("{k} ({v})")).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    let model = s["embedding_model"].as_str().unwrap_or("");
    println!("database     {}  ({})", s["database"].as_str().unwrap_or(""), human_bytes(s["bytes"].as_u64().unwrap_or(0)));
    println!(
        "records      {}  ({} facts · {} procedures · {} knowledge · {} contexts)",
        s["records"], s["facts"], s["procedures"], s["knowledge"], s["contexts"]
    );
    if let Some(types) = s["types"].as_object().filter(|m| !m.is_empty()) {
        let list: Vec<String> = types.iter().map(|(k, v)| format!("{k} ({v})")).collect();
        println!("types        {}", list.join(", "));
    }
    println!("collections  {}", if collections.is_empty() { "—".to_string() } else { collections });
    if model.is_empty() {
        println!("embeddings   none  (full-text only; run `fds embed` to add)");
    } else {
        println!(
            "embeddings   {model}, {} dims — {}/{} records",
            s["embedding_dims"], s["embedded_records"], s["records"]
        );
    }
    println!("updated      {}", s["updated"].as_str().unwrap_or(""));
}

fn short(id: &str) -> &str {
    &id[..id.len().min(8)]
}

pub fn print_hit(h: &Hit) {
    let head = match &h.question {
        Some(q) if h.kind != Kind::Knowledge => q.as_str(),
        _ => h.title.as_str(),
    };
    let kind = if h.rtype.is_empty() { h.kind.as_str().to_string() } else { format!("{}/{}", h.kind.as_str(), h.rtype) };
    println!("[{kind}] {}  {}", short(&h.id), head);
    for line in h.body.lines() {
        println!("    {line}");
    }
    // An image's file and alt text, a price's currency: what the caller needs
    // to use the record, not only to read it.
    if !h.attrs.is_empty() {
        let attrs: Vec<String> = h.attrs.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!("    — attrs: {}", attrs.join(" · "));
    }
    println!(
        "    — source: {} · verified: {} {} · match: {} ({})",
        h.source, h.verified_by, h.last_verified, h.match_kind, h.score
    );
    println!();
}

pub fn print_result(r: &QueryResult) {
    println!("confidence: {}", r.confidence.as_str());
    println!();
    for h in &r.records {
        print_hit(h);
    }
    if r.records.is_empty() {
        println!("(no records)");
    }
}

pub fn print_record(r: &Record) {
    println!("id            {}", r.id);
    println!("kind          {}", r.kind.as_str());
    println!("collection    {}", r.collection);
    if let Some(q) = &r.question {
        println!("question      {q}");
    }
    println!("title         {}", r.title);
    println!("source        {}", r.source);
    println!("verified_by   {}", r.verified_by);
    println!("last_verified {}", r.last_verified);
    println!("tokens        {}", r.tokens);
    println!("embedding     {}", r.embedding.as_ref().map_or("none".to_string(), |e| format!("{} dims", e.len())));
    println!();
    println!("{}", r.body);
}

pub fn print_row(r: &Record) {
    let label = r.question.as_deref().unwrap_or(&r.title);
    let label: String = label.chars().take(70).collect();
    println!("{}  {:<9}  {:<14}  {}", short(&r.id), r.kind.as_str(), r.collection, label);
}

pub fn print_context(b: &ContextBundle) {
    println!("context  {}  ({} records, {} tokens)", b.name, b.records.len(), b.tokens);
    println!("hash     {}", b.hash);
    println!();
    if !b.instructions.is_empty() {
        for line in b.instructions.lines() {
            println!("    {line}");
        }
        println!();
    }
    for h in &b.records {
        print_hit(h);
    }
    if !b.missing.is_empty() {
        println!("links to records that do not exist yet: {}", b.missing.join(", "));
    }
}

/// An import plan for a terminal: every row that does something, then the
/// totals. Unchanged rows are counted, not listed.
pub fn print_plan(p: &crate::etl::Plan) {
    use crate::etl::Status;
    for r in &p.rows {
        let mark = match r.status {
            Status::New => "+ new      ",
            Status::Replace => "~ replace  ",
            Status::Duplicate => "= duplicate",
            Status::Error => "✗ error    ",
            Status::Unchanged => continue,
        };
        let note = r.message.as_deref().map(|m| format!("  — {m}")).unwrap_or_default();
        println!("{mark}  row {:<4} {}{note}", r.row, r.label);
    }
    let s = &p.summary;
    println!();
    println!(
        "new {} · replaced {} · unchanged {} · duplicates {} · errors {}{}",
        s.new,
        s.replace,
        s.unchanged,
        s.duplicate,
        s.error,
        if p.dry_run { "   (dry run: nothing written)" } else if p.written { "" } else { "   (nothing to write)" }
    );
}

