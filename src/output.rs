//! Rendering records and query results for the terminal or as JSON.

use fds::v4::Record;
use serde_json::{json, Value};

/// Convert an internal record into the stable JSON shape used by CLI output.
pub fn public_record(r: &Record) -> Value {
    json!({
        "id": r.id,
        "kind": r.kind.as_str(),
        "collection": r.collection,
        "question": r.question,
        "title": r.title,
        "body": r.body,
        "source": r.source,
        "verified_by": r.verified_by,
        "last_verified": r.last_verified,
        "tokens": r.tokens,
        "type": r.rtype,
        "attrs": r.attrs,
        "links": r.links,
        "embedding_dims": r.embedding.as_ref().map(Vec::len),
    })
}

/// Print a value as pretty JSON, or as tab-separated record lines.
pub fn print_value(v: Value, json_out: bool) {
    if json_out {
        println!("{}", pretty(&v));
        return;
    }
    if let Some(results) = v.get("results").and_then(Value::as_array) {
        println!(
            "confidence: {}",
            v.get("confidence")
                .and_then(Value::as_str)
                .unwrap_or("none")
        );
        for row in results {
            print_record_line(row);
        }
    } else if let Some(rows) = v.as_array() {
        if rows.is_empty() {
            println!("(no records)");
        }
        for row in rows {
            print_record_line(row);
        }
    } else if v.get("id").is_some() {
        print_record_line(&v);
    } else {
        println!("{}", pretty(&v));
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap()
}

fn print_record_line(row: &Value) {
    let text = |key: &str| row.get(key).and_then(Value::as_str);
    let id = text("id").unwrap_or("-");
    let kind = text("kind").unwrap_or("record");
    let name = text("question").or_else(|| text("title")).unwrap_or("-");
    let body = text("body").unwrap_or("");
    println!("{id}\t{kind}\t{name}\t{body}");
}
