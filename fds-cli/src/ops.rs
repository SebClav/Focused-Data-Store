//! Write operations shared by the CLI and the HTTP manager.
//!
//! Both front ends call these same functions, so `fds fact add` on the
//! command line and "New fact" in the browser cannot drift apart: there is
//! one definition of what adding a fact means.

use std::path::Path;

use fds::v4::{DbMeta, Kind, Record};
use serde_json::{json, Value};

use crate::text::{canonical, stable_id, today, word_count};
use crate::{ingest, ollama};

pub const DEFAULT_EMBED_MODEL: &str = "all-minilm";

/// A record as JSON for listings: everything except the embedding vector,
/// which is replaced by its dimension count.
pub fn record_json(r: &Record) -> Value {
    json!({
        "id": r.id, "kind": r.kind, "collection": r.collection, "question": r.question,
        "title": r.title, "body": r.body, "source": r.source, "verified_by": r.verified_by,
        "last_verified": r.last_verified, "tokens": r.tokens,
        "embedding_dims": r.embedding.as_ref().map(|e| e.len()),
        "type": r.rtype, "attrs": r.attrs, "links": r.links,
    })
}

/// Builds a fact without inserting it, so a caller can see its id -- and
/// whether it would be new or replace something -- before writing anything.
pub fn fact_record(
    question: &str,
    answer: &str,
    collection: &str,
    source: &str,
    verified_by: &str,
    date: &str,
) -> Result<Record, String> {
    let (question, answer) = (question.trim(), answer.trim());
    if question.is_empty() || answer.is_empty() {
        return Err("a fact needs both a question and an answer".into());
    }
    let canon = canonical(question);
    if canon.is_empty() {
        return Err(format!(
            "question `{question}` has no words left after normalisation"
        ));
    }
    Ok(Record {
        id: stable_id(&format!("fact:{canon}")),
        kind: Kind::Fact,
        collection: collection.to_string(),
        question: Some(canon),
        body: answer.to_string(),
        title: question.to_string(),
        source: source.to_string(),
        verified_by: verified_by.to_string(),
        last_verified: date.to_string(),
        tokens: word_count(answer),
        embedding: None,
        rtype: String::new(),
        attrs: Default::default(),
        links: Vec::new(),
    })
}

/// The stored fact that saving `incoming` would replace with a different
/// answer, source or collection.
///
/// A fact's id comes from its question alone (SPEC.md §3.1), so the same
/// question in another collection is the same fact, not a second one.
pub fn replaced_fact<'a>(records: &'a [Record], incoming: &Record) -> Option<&'a Record> {
    records.iter().find(|x| {
        x.id == incoming.id
            && x.kind == Kind::Fact
            && (x.body != incoming.body
                || x.source != incoming.source
                || x.collection != incoming.collection)
    })
}

/// The warning for a replacement found by [`replaced_fact`].
pub fn replaced_warning(existing: &Record, incoming: &Record) -> String {
    format!(
        "warning: fact {} already exists in collection `{}` with a different answer or source; \
         the new fact in `{}` replaces it. A question is one fact per database, whatever its \
         collection; give an agent its own sector for separate knowledge.",
        existing.id, existing.collection, incoming.collection
    )
}

/// Adds a fact, or replaces the one with the same canonical question.
/// Returns its stable id.
pub fn upsert_fact(
    records: &mut Vec<Record>,
    question: &str,
    answer: &str,
    collection: &str,
    source: &str,
    verified_by: &str,
    date: &str,
) -> Result<String, String> {
    let r = fact_record(question, answer, collection, source, verified_by, date)?;
    let id = r.id.clone();
    records.retain(|x| x.id != id);
    records.push(r);
    Ok(id)
}

/// What `import_facts` did.
pub struct Imported {
    pub count: usize,
    /// One warning for each fact that replaced a different one.
    pub warnings: Vec<String>,
}

/// Accepts `[{question, answer, source?}]` or the Phase 1 mask format
/// `{"entries": [{canonical_query, answer, source?, last_verified?}]}`.
pub fn import_facts(
    file: &Path,
    records: &mut Vec<Record>,
    collection: &str,
) -> Result<Imported, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{}: invalid JSON: {e}", file.display()))?;
    let items = v
        .get("entries")
        .and_then(|e| e.as_array())
        .or_else(|| v.as_array())
        .ok_or("expected a JSON array of facts, or an object with an `entries` array")?;
    let mut n = 0;
    let mut warnings = Vec::new();
    for item in items {
        let q = item
            .get("question")
            .or_else(|| item.get("canonical_query"))
            .and_then(|x| x.as_str());
        let a = item.get("answer").and_then(|x| x.as_str());
        let (Some(q), Some(a)) = (q, a) else {
            return Err(format!(
                "fact #{} needs `question` (or `canonical_query`) and `answer`",
                n + 1
            ));
        };
        let source = item
            .get("source")
            .and_then(|x| x.as_str())
            .unwrap_or("import");
        let date = item
            .get("last_verified")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .unwrap_or_else(today);
        let fact = fact_record(q, a, collection, source, "human", &date)
            .map_err(|e| format!("fact #{}: {e}", n + 1))?;
        if let Some(existing) = replaced_fact(records, &fact) {
            warnings.push(replaced_warning(existing, &fact));
        }
        upsert(records, fact);
        n += 1;
    }
    Ok(Imported { count: n, warnings })
}

/// Procedures from a JSON file: one object or an array of them.
pub fn add_procedures(
    file: &Path,
    records: &mut Vec<Record>,
    collection: &str,
) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| format!("{}: invalid JSON: {e}", file.display()))?;
    add_procedures_value(v, records, collection, &ingest::source_name(file))
}

/// Procedures from an already-parsed value. `default_source` is used for any
/// procedure that does not name its own `source`.
///
/// Validates everything before touching `records`: a batch with one bad
/// procedure changes nothing, rather than landing half of it.
pub fn add_procedures_value(
    v: Value,
    records: &mut Vec<Record>,
    collection: &str,
    default_source: &str,
) -> Result<Vec<String>, String> {
    let items: Vec<Value> = match v {
        Value::Array(a) => a,
        obj @ Value::Object(_) => vec![obj],
        _ => return Err("expected a procedure object or an array of them".into()),
    };
    if items.is_empty() {
        return Err("no procedures given".into());
    }
    let staged: Vec<Record> = items
        .into_iter()
        .enumerate()
        .map(|(n, item)| {
            procedure_record(item, collection, default_source)
                .map_err(|e| format!("procedure #{}: {e}", n + 1))
        })
        .collect::<Result<_, _>>()?;

    let ids: Vec<String> = staged.iter().map(|r| r.id.clone()).collect();
    records.retain(|r| !ids.contains(&r.id));
    records.extend(staged);
    Ok(ids)
}

/// Builds one procedure without inserting it (see `fact_record`).
///
/// Besides the canonical shape `{intent, steps: [...]}` it accepts the two
/// shapes a spreadsheet produces: `steps` as JSON text inside a cell, and a
/// bare `command` (plus optional `shell`), which becomes a single run step.
/// `verified_by` and `last_verified` are kept when given, so an export
/// re-imported unchanged keeps its provenance instead of being re-dated.
pub fn procedure_record(
    mut item: Value,
    collection: &str,
    default_source: &str,
) -> Result<Record, String> {
    let obj = item.as_object_mut().ok_or("not an object")?;
    let take = |obj: &mut serde_json::Map<String, Value>, k: &str| -> Option<String> {
        obj.remove(k)
            .and_then(|x| x.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
    };

    let intent = take(obj, "intent").ok_or("needs an `intent`")?;
    obj.remove("kind");
    let (rtype, attrs, links) = shape_fields(obj)?;

    if let Some(Value::String(text)) = obj.get("steps") {
        let parsed: Value =
            serde_json::from_str(text).map_err(|e| format!("`steps` is not valid JSON: {e}"))?;
        obj.insert("steps".into(), parsed);
    }
    let command = take(obj, "command");
    let shell = take(obj, "shell");
    if !obj.contains_key("steps") {
        if let Some(cmd) = command {
            let mut step = serde_json::Map::new();
            step.insert("action".into(), json!("run"));
            step.insert("command".into(), json!(cmd));
            if let Some(sh) = shell {
                step.insert("shell".into(), json!(sh));
            }
            obj.insert("steps".into(), json!([step]));
        }
    }
    if !obj
        .get("steps")
        .map_or(false, |s| s.as_array().map_or(false, |a| !a.is_empty()))
    {
        return Err(format!(
            "`{intent}` needs a non-empty `steps` array, or a `command` (SPEC.md §3.2)"
        ));
    }

    let title = take(obj, "title").unwrap_or_else(|| intent.clone());
    let coll = take(obj, "collection").unwrap_or_else(|| collection.to_string());
    let source = take(obj, "source").unwrap_or_else(|| default_source.to_string());
    let verified_by = take(obj, "verified_by").unwrap_or_else(|| "human".to_string());
    let last_verified = take(obj, "last_verified").unwrap_or_else(today);
    let body = serde_json::to_string_pretty(&Value::Object(obj.clone())).unwrap_or_default();

    let canon = canonical(&intent);
    if canon.is_empty() {
        return Err(format!(
            "intent `{intent}` has no words left after normalisation"
        ));
    }
    Ok(Record {
        id: stable_id(&format!("procedure:{canon}")),
        kind: Kind::Procedure,
        collection: coll,
        question: Some(canon),
        tokens: word_count(&body),
        body,
        title,
        source,
        verified_by,
        last_verified,
        embedding: None,
        rtype,
        attrs,
        links,
    })
}

/// Removes `type`, `attrs` and `links` from a row and returns them typed.
/// They arrive as a spreadsheet produces them as easily as from JSON: `attrs`
/// as an object or its JSON text, `links` as an array, its JSON text, or a
/// plain list separated by `;` or new lines.
pub fn shape_fields(
    obj: &mut serde_json::Map<String, Value>,
) -> Result<
    (
        String,
        std::collections::BTreeMap<String, String>,
        Vec<String>,
    ),
    String,
> {
    let rtype = obj
        .remove("type")
        .and_then(|v| v.as_str().map(|s| s.trim().to_string()))
        .unwrap_or_default();

    let attrs_v = match obj.remove("attrs") {
        Some(Value::String(t)) if t.trim().is_empty() => Value::Null,
        Some(Value::String(t)) => {
            serde_json::from_str(&t).map_err(|e| format!("`attrs` is not valid JSON: {e}"))?
        }
        Some(v) => v,
        None => Value::Null,
    };
    let mut attrs = std::collections::BTreeMap::new();
    match attrs_v {
        Value::Null => {}
        Value::Object(m) => {
            for (k, v) in m {
                let v = match v {
                    Value::String(s) => s,
                    Value::Null => continue,
                    other => other.to_string(),
                };
                attrs.insert(k, v);
            }
        }
        _ => return Err("`attrs` must be an object of names to values".into()),
    }

    let links = match obj.remove("links") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .collect(),
        Some(Value::String(t)) => {
            let t = t.trim();
            if t.starts_with('[') {
                serde_json::from_str::<Vec<String>>(t)
                    .map_err(|e| format!("`links` is not a valid JSON list: {e}"))?
            } else {
                t.split(|c| c == ';' || c == '\n')
                    .map(|s| s.trim().to_string())
                    .collect()
            }
        }
        Some(_) => return Err("`links` must be a list of names".into()),
    };
    let links = links.into_iter().filter(|l| !l.is_empty()).collect();
    Ok((rtype, attrs, links))
}

/// Builds a context (SPEC §4.4): a named set of instructions plus the
/// records it links to. Its links can be given here, written as `[[links]]`
/// in the instructions, or both. The id follows the name, so saving a context
/// under the same name replaces it.
pub fn context_record(
    name: &str,
    instructions: &str,
    links: &[String],
    collection: &str,
    source: &str,
    verified_by: &str,
    date: &str,
) -> Result<Record, String> {
    let name = name.trim();
    let canon = canonical(name);
    if canon.is_empty() {
        return Err("a context needs a name".into());
    }
    if instructions.trim().is_empty() && links.is_empty() {
        return Err(format!(
            "context `{name}` is empty: give it instructions, links, or both"
        ));
    }
    Ok(Record {
        id: stable_id(&format!("context:{canon}")),
        kind: Kind::Knowledge,
        collection: collection.to_string(),
        question: None,
        body: instructions.trim().to_string(),
        title: name.to_string(),
        source: source.to_string(),
        verified_by: verified_by.to_string(),
        last_verified: date.to_string(),
        tokens: word_count(instructions),
        embedding: None,
        rtype: crate::query::CONTEXT_TYPE.to_string(),
        attrs: Default::default(),
        links: links
            .iter()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    })
}

/// Inserts `r`, replacing any record with the same id. Returns the id.
pub fn upsert(records: &mut Vec<Record>, r: Record) -> String {
    let id = r.id.clone();
    records.retain(|x| x.id != id);
    records.push(r);
    id
}

/// A change to one record. Every field is optional; what is absent stays.
#[derive(serde::Deserialize, Default, Debug)]
pub struct Edit {
    pub body: Option<String>,
    pub title: Option<String>,
    #[serde(rename = "type")]
    pub rtype: Option<String>,
    pub attrs: Option<std::collections::BTreeMap<String, String>>,
    pub links: Option<Vec<String>>,
    /// Mark the record as checked by a person without changing it.
    #[serde(default)]
    pub verify: bool,
}

/// Applies an edit made by a person. The question or intent is not
/// editable: it is what the id is made from, so changing it is a new record.
///
/// An edit is a verification: the record becomes `verified_by: human`, dated
/// today. If its text changes, its vector no longer describes it and is
/// dropped, to be computed again by `embed`.
pub fn edit(records: &mut [Record], id: &str, e: Edit, date: &str) -> Result<String, String> {
    let touched = e.body.is_some()
        || e.title.is_some()
        || e.rtype.is_some()
        || e.attrs.is_some()
        || e.links.is_some();
    if !touched && !e.verify {
        return Err("nothing to change: give a field, or `verify: true`".into());
    }
    let i = crate::db::resolve_id(records, id)?;
    let r = &mut records[i];
    let mut changed_text = false;

    if let Some(body) = e.body {
        let body = body.trim().to_string();
        match r.kind {
            _ if body.is_empty() && r.rtype != crate::query::CONTEXT_TYPE => {
                return Err("the content cannot be empty".into())
            }
            Kind::Procedure => {
                let v: Value = serde_json::from_str(&body)
                    .map_err(|e| format!("a procedure's content must be JSON: {e}"))?;
                if !v
                    .get("steps")
                    .map_or(false, |s| s.as_array().map_or(false, |a| !a.is_empty()))
                {
                    return Err("a procedure needs a non-empty `steps` array (SPEC.md §3.2)".into());
                }
            }
            _ => {}
        }
        if body != r.body {
            r.tokens = word_count(&body);
            r.body = body;
            changed_text = true;
        }
    }
    if let Some(title) = e
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        if r.rtype == crate::query::CONTEXT_TYPE && canonical(&title) != canonical(&r.title) {
            return Err(
                "a context's name is what its id is made from; save it under the new name instead"
                    .into(),
            );
        }
        r.title = title;
    }
    if let Some(t) = e.rtype {
        let t = t.trim().to_string();
        if (t == crate::query::CONTEXT_TYPE) != (r.rtype == crate::query::CONTEXT_TYPE) {
            return Err(
                "a record cannot become a context, or stop being one; create a context instead"
                    .into(),
            );
        }
        r.rtype = t;
    }
    if let Some(a) = e.attrs {
        r.attrs = a;
    }
    if let Some(l) = e.links {
        r.links = l
            .into_iter()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
    }
    if changed_text {
        r.embedding = None;
    }
    // Editing or verifying: either way a person vouches for the record now.
    r.verified_by = "human".to_string();
    r.last_verified = date.to_string();
    Ok(r.id.clone())
}

/// Removes one record by full id or unambiguous prefix. Returns its id.
pub fn remove(records: &mut Vec<Record>, id: &str) -> Result<String, String> {
    let i = crate::db::resolve_id(records, id)?;
    Ok(records.remove(i).id)
}

/// Embeds records that lack a vector (or every record, with `all`), using the
/// database's model or `model`. Returns how many were embedded.
///
/// Refuses to mix models: vectors from two models are not comparable, and a
/// database holding both would rank on noise.
pub async fn embed(
    meta: &mut DbMeta,
    records: &mut [Record],
    model: Option<String>,
    all: bool,
) -> Result<usize, String> {
    let model = model.unwrap_or_else(|| {
        if meta.embedding_model.is_empty() {
            DEFAULT_EMBED_MODEL.to_string()
        } else {
            meta.embedding_model.clone()
        }
    });
    if !meta.embedding_model.is_empty() && meta.embedding_model != model && !all {
        return Err(format!(
            "database is embedded with `{}`; mixing models breaks similarity. Use --all to re-embed everything with `{model}`.",
            meta.embedding_model
        ));
    }
    let todo: Vec<usize> = (0..records.len())
        .filter(|&i| all || records[i].embedding.is_none())
        .collect();
    if todo.is_empty() {
        return Ok(0);
    }
    // Compiled records are matched on their question, so that is what gets
    // embedded; knowledge is matched on its body.
    let texts: Vec<String> = todo
        .iter()
        .map(|&i| {
            records[i]
                .question
                .clone()
                .unwrap_or_else(|| records[i].body.clone())
        })
        .collect();
    let vectors = ollama::embed(&model, &texts).await?;
    let dims = vectors.first().map(|v| v.len()).unwrap_or(0) as u32;
    for (&i, v) in todo.iter().zip(vectors) {
        records[i].embedding = Some(v);
    }
    meta.embedding_model = model;
    meta.embedding_dims = dims;
    Ok(todo.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_procedure_in_a_batch_changes_nothing() {
        let mut records = Vec::new();
        let batch = json!([
            {"intent": "flush the dns cache", "steps": [{"action": "run", "command": "ipconfig /flushdns"}]},
            {"intent": "broken one", "steps": []}
        ]);
        let err = add_procedures_value(batch, &mut records, "default", "test").unwrap_err();
        assert!(
            err.contains("broken one"),
            "error should name the bad procedure: {err}"
        );
        assert!(
            records.is_empty(),
            "a rejected batch must not land partially"
        );
    }

    #[test]
    fn re_adding_a_procedure_replaces_it() {
        let mut records = Vec::new();
        let one = |cmd: &str| json!({"intent": "Flush the DNS cache", "steps": [{"action": "run", "command": cmd}]});
        let a = add_procedures_value(one("old"), &mut records, "default", "t").unwrap();
        let b =
            add_procedures_value(one("ipconfig /flushdns"), &mut records, "default", "t").unwrap();
        assert_eq!(a, b, "same intent must keep the same stable id");
        assert_eq!(records.len(), 1);
        assert!(records[0].body.contains("ipconfig /flushdns"));
    }

    fn fact(question: &str, answer: &str, collection: &str, source: &str) -> Record {
        fact_record(question, answer, collection, source, "human", "2026-10-29").unwrap()
    }

    #[test]
    fn a_different_answer_to_the_same_question_replaces_a_fact() {
        let stored = [fact("What is the port?", "443", "alpha", "s")];
        let incoming = fact("what is the PORT", "8080", "alpha", "s");
        assert_eq!(
            replaced_fact(&stored, &incoming).map(|r| r.body.as_str()),
            Some("443")
        );
    }

    #[test]
    fn the_same_question_in_another_collection_is_the_same_fact() {
        // Identifiers come from the question alone (SPEC.md 3.1).
        let stored = [fact("What is the port?", "443", "alpha", "s")];
        let incoming = fact("What is the port?", "443", "beta", "s");
        assert_eq!(stored[0].id, incoming.id);
        assert!(replaced_fact(&stored, &incoming).is_some());
        let warning = replaced_warning(&stored[0], &incoming);
        assert!(
            warning.contains("`alpha`") && warning.contains("`beta`"),
            "{warning}"
        );
    }

    #[test]
    fn saving_the_same_fact_again_replaces_nothing_different() {
        let stored = [fact("What is the port?", "443", "alpha", "s")];
        assert!(replaced_fact(&stored, &fact("What is the port?", "443", "alpha", "s")).is_none());
    }

    #[test]
    fn another_question_or_another_kind_is_not_replaced() {
        let stored = [fact("What is the port?", "443", "alpha", "s")];
        assert!(replaced_fact(&stored, &fact("What is the host?", "x", "alpha", "s")).is_none());
        let mut procedure = stored[0].clone();
        procedure.kind = Kind::Procedure;
        let incoming = fact("What is the port?", "8080", "alpha", "s");
        assert!(replaced_fact(&[procedure], &incoming).is_none());
    }

    #[test]
    fn remove_accepts_a_prefix() {
        let mut records = Vec::new();
        let id = upsert_fact(
            &mut records,
            "what is pin 4 of the ne555",
            "RESET",
            "d",
            "s",
            "human",
            "2026-09-25",
        )
        .unwrap();
        assert_eq!(remove(&mut records, &id[..8]).unwrap(), id);
        assert!(records.is_empty());
    }

    #[test]
    fn an_edit_is_a_verification_and_drops_a_stale_vector() {
        let mut records = Vec::new();
        let mut img = crate::media::image_record(
            "a.png",
            "d",
            b"x",
            "image/png",
            "A router.",
            "qwen2.5vl:3b",
        );
        img.embedding = Some(vec![0.1, 0.2]);
        let id = upsert(&mut records, img);

        // Verify only: same text, the vector stays.
        edit(
            &mut records,
            &id,
            Edit {
                verify: true,
                ..Default::default()
            },
            "2026-09-26",
        )
        .unwrap();
        assert_eq!(
            (
                records[0].verified_by.as_str(),
                records[0].last_verified.as_str()
            ),
            ("human", "2026-09-26")
        );
        assert!(records[0].embedding.is_some());

        // Change the text: the vector no longer describes it.
        edit(
            &mut records,
            &id,
            Edit {
                body: Some("TP-Link Archer C6, back panel.".into()),
                ..Default::default()
            },
            "2026-09-26",
        )
        .unwrap();
        assert!(records[0].embedding.is_none());
        assert_eq!(records[0].tokens, 5);
    }

    #[test]
    fn an_edit_cannot_break_a_procedure_or_turn_a_record_into_a_context() {
        let mut records = Vec::new();
        let ids = add_procedures_value(
            json!({"intent": "flush dns", "command": "ipconfig /flushdns"}),
            &mut records,
            "d",
            "t",
        )
        .unwrap();
        let bad = edit(
            &mut records,
            &ids[0],
            Edit {
                body: Some("not json".into()),
                ..Default::default()
            },
            "2026-09-26",
        );
        assert!(bad.unwrap_err().contains("JSON"));
        let no_steps = edit(
            &mut records,
            &ids[0],
            Edit {
                body: Some("{\"steps\": []}".into()),
                ..Default::default()
            },
            "2026-09-26",
        );
        assert!(no_steps.is_err());
        let ctx = edit(
            &mut records,
            &ids[0],
            Edit {
                rtype: Some("context".into()),
                ..Default::default()
            },
            "2026-09-26",
        );
        assert!(ctx.is_err());
        assert!(
            records[0].body.contains("ipconfig"),
            "a refused edit changes nothing"
        );
    }
}
