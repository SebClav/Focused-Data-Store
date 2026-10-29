//! Bulk import and export (SPEC.md §8.3).
//!
//! Import is a plan followed, optionally, by applying it. Both come out of
//! the same function, so the preview a person reviews and the write that
//! follows cannot disagree: the dry run is the real run with the save left
//! out.
//!
//! Export writes rows in exactly the shape import reads, so a database can
//! go out to a spreadsheet, be edited there, and come back -- and rows nobody
//! touched come back `unchanged`, keeping their original verification date.

use std::collections::HashMap;

use fds::v4::{Kind, Record};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::text::today;
use crate::{ingest, media, ops};

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum OnError {
    /// Any invalid row cancels the whole import. The default: a batch lands
    /// completely or not at all.
    #[default]
    Abort,
    /// Invalid rows are reported and left out; the rest is written.
    Skip,
}

#[derive(Deserialize, Default)]
pub struct ImportRequest {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub on_error: OnError,
    /// Collection for rows and documents that name none.
    #[serde(default)]
    pub collection: Option<String>,
    /// Source for rows that name none -- typically the uploaded file's name.
    #[serde(default)]
    pub source: Option<String>,
    /// Facts and procedures, one object each (SPEC §8.3 for the fields).
    #[serde(default)]
    pub rows: Vec<Value>,
    /// Whole documents, split into knowledge exactly as `fds add` splits files.
    #[serde(default)]
    pub documents: Vec<Document>,
    /// Vision model for images; `FDS_VISION_MODEL` or the default otherwise.
    #[serde(default)]
    pub vision_model: Option<String>,
    /// Language to describe images in, as a code: `es`, `en`, ...
    #[serde(default)]
    pub lang: Option<String>,
}

#[derive(Deserialize)]
pub struct Document {
    pub name: String,
    /// The document's text. Empty for an image, which comes as `data`.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub collection: Option<String>,
    /// An image, base64-encoded (SPEC §3.3). FDS keeps a description of it,
    /// never the image itself.
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub media_type: Option<String>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    New,
    Replace,
    Unchanged,
    Duplicate,
    Error,
}

#[derive(Serialize, Debug)]
pub struct RowPlan {
    /// 1-based, matching the row a person sees in their spreadsheet.
    pub row: usize,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    pub label: String,
    /// Stable identifier of what happened, for clients that show it in their
    /// own language. `message` says the same in English, for everyone else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    /// The one variable part of `code` -- a row number, a field, a value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Why a row or document cannot be imported: a stable code, its variable
/// part, and the same thing in English.
#[derive(Debug)]
struct Problem {
    code: &'static str,
    arg: Option<String>,
    message: String,
}

fn problem(code: &'static str, arg: Option<String>, message: impl Into<String>) -> Problem {
    Problem { code, arg, message: message.into() }
}

#[derive(Serialize, Debug)]
pub struct DocPlan {
    pub name: String,
    pub status: Status,
    pub fragments: usize,
    /// Knowledge records from this source that the import would replace.
    pub replaces: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Serialize, Default, Debug)]
pub struct Summary {
    pub new: usize,
    pub replace: usize,
    pub unchanged: usize,
    pub duplicate: usize,
    pub error: usize,
    /// Knowledge fragments produced by the documents.
    pub fragments: usize,
}

#[derive(Serialize, Debug)]
pub struct Plan {
    pub dry_run: bool,
    pub on_error: &'static str,
    pub written: bool,
    pub summary: Summary,
    pub rows: Vec<RowPlan>,
    pub documents: Vec<DocPlan>,
}

impl Plan {
    /// True when the plan was refused because of errors under `abort`.
    pub fn blocked(&self) -> bool {
        !self.dry_run && !self.written && self.summary.error > 0 && self.on_error == "abort"
    }
}

/// Everything a record carries except provenance and vectors. Two records
/// that agree on this are the same record for import purposes; importing one
/// over the other changes nothing and should not bump its date.
fn same_content(a: &Record, b: &Record) -> bool {
    a.kind == b.kind
        && a.body == b.body
        && a.title == b.title
        && a.collection == b.collection
        && a.source == b.source
        && a.rtype == b.rtype
        && a.attrs == b.attrs
        && a.links == b.links
}

/// What an import row describes. Contexts are knowledge to the engine, but
/// rows name them on their own.
#[derive(Clone, Copy, PartialEq)]
enum RowKind {
    Fact,
    Procedure,
    Context,
}

fn kind_of(row: &Value) -> Result<RowKind, Problem> {
    match row.get("kind").and_then(|k| k.as_str()).map(|k| k.trim().to_lowercase()).as_deref() {
        Some("fact") | Some("hecho") => Ok(RowKind::Fact),
        Some("procedure") | Some("procedimiento") => Ok(RowKind::Procedure),
        Some("context") | Some("contexto") => Ok(RowKind::Context),
        Some("knowledge") | Some("conocimiento") => Err(problem(
            "knowledge_row",
            None,
            "knowledge comes from documents; import the file itself instead of a row",
        )),
        Some("") | None => Err(problem("missing_kind", None, "needs a `kind`: fact, procedure or context")),
        Some(other) => Err(problem(
            "unknown_kind",
            Some(other.to_string()),
            format!("unknown kind `{other}`: expected fact, procedure or context"),
        )),
    }
}

fn str_field<'a>(row: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| row.get(*k).and_then(|v| v.as_str())).map(str::trim).filter(|s| !s.is_empty())
}

/// The checks a person can act on are made here, with a code each; whatever
/// `ops` still rejects after them is reported as `invalid`, in its words.
fn build(row: &Value, collection: &str, source: &str) -> Result<Record, Problem> {
    let invalid = |e: String| problem("invalid", Some(e.clone()), e);
    let shape = |row: &Value| {
        let mut o = row.as_object().cloned().unwrap_or_default();
        ops::shape_fields(&mut o).map_err(|e| problem("invalid", Some(e.clone()), e))
    };
    match kind_of(row)? {
        RowKind::Fact => {
            let q = str_field(row, &["question", "canonical_query"])
                .ok_or_else(|| problem("missing_question", None, "needs a `question`"))?;
            let a = str_field(row, &["answer"]).ok_or_else(|| problem("missing_answer", None, "needs an `answer`"))?;
            let coll = str_field(row, &["collection"]).unwrap_or(collection);
            let src = str_field(row, &["source"]).unwrap_or(source);
            let by = str_field(row, &["verified_by"]).unwrap_or("human");
            let date = str_field(row, &["last_verified"]).map(str::to_string).unwrap_or_else(today);
            let (rtype, attrs, links) = shape(row)?;
            let mut rec = ops::fact_record(q, a, coll, src, by, &date).map_err(invalid)?;
            (rec.rtype, rec.attrs, rec.links) = (rtype, attrs, links);
            Ok(rec)
        }
        RowKind::Context => {
            let name = str_field(row, &["name", "title"])
                .ok_or_else(|| problem("missing_name", None, "a context needs a `name`"))?;
            let text = str_field(row, &["instructions", "answer", "body"]).unwrap_or("");
            let (_, _, links) = shape(row)?;
            let coll = str_field(row, &["collection"]).unwrap_or(collection);
            let src = str_field(row, &["source"]).unwrap_or(source);
            let by = str_field(row, &["verified_by"]).unwrap_or("human");
            let date = str_field(row, &["last_verified"]).map(str::to_string).unwrap_or_else(today);
            ops::context_record(name, text, &links, coll, src, by, &date).map_err(invalid)
        }
        RowKind::Procedure => {
            if str_field(row, &["intent"]).is_none() {
                return Err(problem("missing_intent", None, "needs an `intent`"));
            }
            let has_steps = match row.get("steps") {
                Some(Value::Array(a)) => !a.is_empty(),
                Some(Value::String(s)) => !s.trim().is_empty(),
                _ => false,
            };
            if !has_steps && str_field(row, &["command"]).is_none() {
                return Err(problem("missing_steps", None, "needs `steps`, or a `command` to make one step from"));
            }
            ops::procedure_record(row.clone(), collection, source).map_err(|e| {
                if e.contains("not valid JSON") { problem("bad_steps", Some(e.clone()), e) } else { invalid(e) }
            })
        }
    }
}

fn label_of(row: &Value) -> String {
    str_field(row, &["question", "canonical_query", "intent", "name", "title"])
        .map(|s| s.chars().take(120).collect())
        .unwrap_or_else(|| "(no question or intent)".into())
}

/// What a plan would write, held until the plan is allowed to run.
#[derive(Default)]
pub struct Staged {
    records: Vec<Record>,
    docs: Vec<(String, String, Vec<(String, String)>)>,
    images: Vec<PendingImage>,
}

/// An image the plan will describe -- but only on a real run. A preview never
/// calls the model: reviewing an import must cost nothing.
struct PendingImage {
    plan_index: usize,
    name: String,
    collection: String,
    bytes: Vec<u8>,
    media: String,
}

/// Plans and applies an import that holds no images to describe (images in
/// it are reported as errors on a real run). The tests' synchronous entry
/// point; everything else goes through `run_async`.
#[cfg(test)]
pub fn run(req: ImportRequest, records: &mut Vec<Record>) -> Plan {
    let model = req.vision_model.clone().unwrap_or_else(media::vision_model);
    let (mut plan, mut staged) = plan(&req, records, &model);
    if !plan.dry_run {
        for img in std::mem::take(&mut staged.images) {
            fail_image(&mut plan, img.plan_index, "vision_unavailable", "no vision model in this context".into());
        }
    }
    finish(plan, staged, records, req.on_error)
}

/// Plans an import, describes its images with `describe` -- only on a real
/// run, and all of them before anything is written -- then applies it unless
/// errors block it. Returns the plan either way.
pub async fn run_async<F, Fut>(req: ImportRequest, records: &mut Vec<Record>, describe: F) -> Plan
where
    F: Fn(Vec<u8>, String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let model = req.vision_model.clone().unwrap_or_else(media::vision_model);
    let (mut plan, mut staged) = plan(&req, records, &model);
    if !plan.dry_run {
        let prompt = media::prompt(req.lang.as_deref());
        for img in std::mem::take(&mut staged.images) {
            // Under all-or-nothing, one failure already dooms the batch:
            // stop instead of spending minutes on descriptions nobody keeps.
            if plan.summary.error > 0 && req.on_error == OnError::Abort {
                break;
            }
            match describe(img.bytes.clone(), prompt.clone()).await {
                Ok(text) => {
                    let d = &mut plan.documents[img.plan_index];
                    d.code = Some("described");
                    d.message = Some(format!("described by {model}"));
                    staged.records.push(media::image_record(&img.name, &img.collection, &img.bytes, &img.media, &text, &model));
                }
                Err(e) => fail_image(&mut plan, img.plan_index, "vision_failed", e),
            }
        }
    }
    finish(plan, staged, records, req.on_error)
}

fn fail_image(plan: &mut Plan, i: usize, code: &'static str, e: String) {
    let d = &mut plan.documents[i];
    match d.status {
        Status::New => plan.summary.new -= 1,
        Status::Replace => plan.summary.replace -= 1,
        _ => {}
    }
    d.status = Status::Error;
    d.fragments = 0;
    d.code = Some(code);
    d.arg = Some(e.clone());
    d.message = Some(e);
    plan.summary.error += 1;
}

fn finish(mut plan: Plan, staged: Staged, records: &mut Vec<Record>, on_error: OnError) -> Plan {
    let blocked = plan.summary.error > 0 && on_error == OnError::Abort;
    if !plan.dry_run && !blocked {
        for rec in staged.records {
            records.retain(|r| r.id != rec.id);
            records.push(rec);
            plan.written = true;
        }
        for (name, coll, frags) in staged.docs {
            ingest::upsert(records, &name, &coll, frags);
            plan.written = true;
        }
    }
    plan
}

/// The plan alone: what every row and document would do. Writes nothing and
/// calls no model.
fn plan(req: &ImportRequest, records: &[Record], vision_model: &str) -> (Plan, Staged) {
    let collection = req.collection.clone().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "default".into());
    let source = req.source.clone().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "import".into());
    let existing: HashMap<&str, &Record> = records.iter().map(|r| (r.id.as_str(), r)).collect();

    // Rows: build every one first, so duplicates can be judged against the
    // whole file -- a question repeated further down wins, as in a spreadsheet
    // read top to bottom.
    let built: Vec<(usize, Result<Record, Problem>, String)> = req
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| (i + 1, build(row, &collection, &source), label_of(row)))
        .collect();
    let mut last_row_for: HashMap<String, usize> = HashMap::new();
    for (row, r, _) in &built {
        if let Ok(rec) = r {
            last_row_for.insert(rec.id.clone(), *row);
        }
    }

    let mut plans = Vec::with_capacity(built.len());
    let mut to_write: Vec<Record> = Vec::new();
    let mut summary = Summary::default();
    for (row, r, label) in built {
        let plan = match r {
            Err(p) => {
                summary.error += 1;
                RowPlan { row, status: Status::Error, id: None, kind: None, label, code: Some(p.code), arg: p.arg, message: Some(p.message) }
            }
            Ok(rec) => {
                let winner = last_row_for[&rec.id];
                let (status, code, arg, message) = if winner != row {
                    summary.duplicate += 1;
                    let what = match (rec.kind, rec.rtype.as_str()) {
                        (_, crate::query::CONTEXT_TYPE) => "name",
                        (Kind::Fact, _) => "question",
                        _ => "intent",
                    };
                    (Status::Duplicate, Some("duplicate"), Some(winner.to_string()), Some(format!("same {what} as row {winner}, which wins")))
                } else {
                    match existing.get(rec.id.as_str()) {
                        Some(old) if same_content(old, &rec) => {
                            summary.unchanged += 1;
                            (Status::Unchanged, None, None, None)
                        }
                        Some(old) => {
                            summary.replace += 1;
                            let what = if old.body != rec.body {
                                "content"
                            } else if old.links != rec.links {
                                "links"
                            } else if old.rtype != rec.rtype || old.attrs != rec.attrs {
                                "attributes"
                            } else if old.collection != rec.collection {
                                "collection"
                            } else if old.title != rec.title {
                                "title"
                            } else {
                                "source"
                            };
                            (Status::Replace, Some("replace"), Some(what.to_string()), Some(format!("replaces an existing record; its {what} changes")))
                        }
                        None => {
                            summary.new += 1;
                            (Status::New, None, None, None)
                        }
                    }
                };
                let p = RowPlan { row, status, id: Some(rec.id.clone()), kind: Some(rec.kind), label, code, arg, message };
                if matches!(status, Status::New | Status::Replace) {
                    to_write.push(rec);
                }
                p
            }
        };
        plans.push(plan);
    }

    // Documents: split exactly as `fds add` would, then compare with what the
    // same source already holds.
    let mut doc_plans = Vec::with_capacity(req.documents.len());
    let mut docs_to_write: Vec<(String, String, Vec<(String, String)>)> = Vec::new();
    let mut images: Vec<PendingImage> = Vec::new();
    for d in &req.documents {
        let name = d.name.replace('\\', "/");
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
        let coll = d.collection.clone().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| collection.clone());
        let fail = |code: &'static str, arg: Option<String>, m: String| DocPlan {
            name: name.clone(), status: Status::Error, fragments: 0, replaces: 0, code: Some(code), arg, message: Some(m),
        };

        let plan = if let Some(data) = &d.data {
            plan_image(d, data, &name, &ext, &coll, records, vision_model, doc_plans.len(), &mut images)
        } else if d.text.len() > ingest::MAX_DOCUMENT_BYTES {
            let mb = ingest::MAX_DOCUMENT_BYTES / 1_048_576;
            fail("too_large", Some(mb.to_string()), format!("larger than {mb} MB"))
        } else if d.text.contains('\0') {
            fail("binary", None, "looks binary; FDS ingests text".into())
        } else {
            match ingest::fragments_text(&d.text, &name, &ext) {
                None => fail("unsupported", Some(ext.clone()), format!("`.{ext}` is not a format FDS ingests (Markdown, text or source code)")),
                Some(frags) if frags.is_empty() => fail("empty", None, "no text in it".into()),
                Some(frags) => {
                    let old: Vec<&Record> =
                        records.iter().filter(|r| r.kind == Kind::Knowledge && r.source == name).collect();
                    let identical = old.len() == frags.len()
                        && old.iter().zip(&frags).all(|(r, (t, b))| &r.title == t && &r.body == b && r.collection == coll);
                    let status = if old.is_empty() {
                        Status::New
                    } else if identical {
                        Status::Unchanged
                    } else {
                        Status::Replace
                    };
                    let p = DocPlan { name: name.clone(), status, fragments: frags.len(), replaces: old.len(), code: None, arg: None, message: None };
                    if status != Status::Unchanged {
                        summary.fragments += frags.len();
                        docs_to_write.push((name.clone(), coll, frags));
                    }
                    p
                }
            }
        };
        match plan.status {
            Status::Error => summary.error += 1,
            Status::New => summary.new += 1,
            Status::Replace => summary.replace += 1,
            Status::Unchanged => summary.unchanged += 1,
            Status::Duplicate => {}
        }
        doc_plans.push(plan);
    }

    let plan = Plan {
        dry_run: req.dry_run,
        on_error: if req.on_error == OnError::Abort { "abort" } else { "skip" },
        written: false,
        summary,
        rows: plans,
        documents: doc_plans,
    };
    (plan, Staged { records: to_write, docs: docs_to_write, images })
}

/// An image document: decode it, hash it, and decide whether it needs
/// describing. The same bytes under the same name are `unchanged` and are not
/// described again -- which also keeps a description a person has corrected.
#[allow(clippy::too_many_arguments)]
fn plan_image(
    d: &Document,
    data: &str,
    name: &str,
    ext: &str,
    coll: &str,
    records: &[Record],
    vision_model: &str,
    plan_index: usize,
    images: &mut Vec<PendingImage>,
) -> DocPlan {
    use base64::Engine as _;
    let fail = |code: &'static str, arg: Option<String>, m: String| DocPlan {
        name: name.to_string(), status: Status::Error, fragments: 0, replaces: 0, code: Some(code), arg, message: Some(m),
    };
    let media = match d.media_type.as_deref().filter(|m| m.starts_with("image/")).or_else(|| media::media_type(ext)) {
        Some(m) => m.to_string(),
        None => return fail("unsupported", Some(ext.to_string()), format!("`.{ext}` is not an image format FDS describes")),
    };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(data.trim()) {
        Ok(b) if !b.is_empty() => b,
        _ => return fail("bad_image_data", None, "the image data is not valid base64".into()),
    };
    if bytes.len() > media::MAX_IMAGE_BYTES {
        let mb = media::MAX_IMAGE_BYTES / 1_048_576;
        return fail("too_large", Some(mb.to_string()), format!("larger than {mb} MB"));
    }
    let sha = media::sha256_hex(&bytes);
    let old = records.iter().find(|r| r.id == media::image_id(name));
    let unchanged = old.map_or(false, |r| r.attrs.get("sha256") == Some(&sha) && r.collection == coll);
    let status = match (old, unchanged) {
        (_, true) => Status::Unchanged,
        (Some(_), false) => Status::Replace,
        (None, _) => Status::New,
    };
    if status == Status::Unchanged {
        return DocPlan { name: name.to_string(), status, fragments: 1, replaces: 1, code: None, arg: None, message: None };
    }
    images.push(PendingImage { plan_index, name: name.to_string(), collection: coll.to_string(), bytes, media });
    DocPlan {
        name: name.to_string(),
        status,
        fragments: 1,
        replaces: usize::from(old.is_some()),
        code: Some("describe"),
        arg: Some(vision_model.to_string()),
        message: Some(format!("will be described by {vision_model}")),
    }
}

// ── export ────────────────────────────────────────────────────────────────

/// One record in the shape `run` imports. Facts export their question as
/// typed (the title); procedures their canonical intent, which re-imports to
/// the same id.
pub fn export_row(r: &Record) -> Value {
    let mut v = export_core(r);
    if let Some(o) = v.as_object_mut() {
        if !r.rtype.is_empty() && r.rtype != crate::query::CONTEXT_TYPE {
            o.insert("type".into(), json!(r.rtype));
        }
        if !r.attrs.is_empty() {
            o.insert("attrs".into(), json!(r.attrs));
        }
        if !r.links.is_empty() {
            o.insert("links".into(), json!(r.links));
        }
    }
    v
}

fn export_core(r: &Record) -> Value {
    if r.rtype == crate::query::CONTEXT_TYPE {
        return json!({
            "kind": "context", "name": r.title, "instructions": r.body,
            "collection": r.collection, "source": r.source,
            "verified_by": r.verified_by, "last_verified": r.last_verified,
        });
    }
    match r.kind {
        Kind::Fact => json!({
            "kind": "fact", "question": r.title, "answer": r.body,
            "collection": r.collection, "source": r.source,
            "verified_by": r.verified_by, "last_verified": r.last_verified,
        }),
        Kind::Procedure => {
            let mut v = serde_json::from_str::<Value>(&r.body).unwrap_or_else(|_| json!({}));
            let o = v.as_object_mut().expect("procedure bodies are objects");
            o.insert("kind".into(), json!("procedure"));
            o.insert("intent".into(), json!(r.question.clone().unwrap_or_default()));
            o.insert("title".into(), json!(r.title));
            o.insert("collection".into(), json!(r.collection));
            o.insert("source".into(), json!(r.source));
            o.insert("verified_by".into(), json!(r.verified_by));
            o.insert("last_verified".into(), json!(r.last_verified));
            v
        }
        Kind::Knowledge => json!({
            "kind": "knowledge", "title": r.title, "body": r.body,
            "collection": r.collection, "source": r.source,
        }),
    }
}

pub const CSV_COLUMNS: &[&str] = &[
    "kind", "question", "answer", "intent", "title", "steps", "type", "attrs", "links",
    "collection", "source", "verified_by", "last_verified",
];

/// Facts, procedures and contexts as CSV, with a UTF-8 byte-order mark so
/// Excel reads accents correctly. A context puts its name in `title` and its
/// instructions in `answer`. Knowledge is left out: it is documents, not rows,
/// and a row of it could not be imported back.
///
/// Cells that a spreadsheet would run as a formula (leading `=`, `+`, `-`,
/// `@`, tab or carriage return) get a leading apostrophe, and import strips
/// exactly that apostrophe back off -- so an answer like `-5 V` survives the
/// round trip, and a record can never smuggle a formula into someone's Excel.
pub fn export_csv(records: &[&Record], delimiter: char) -> String {
    let mut out = String::from('\u{FEFF}');
    out.push_str(&CSV_COLUMNS.join(&delimiter.to_string()));
    out.push_str("\r\n");
    for r in records.iter().filter(|r| r.kind != Kind::Knowledge || r.rtype == crate::query::CONTEXT_TYPE) {
        let mut v = export_row(r);
        if let Some(o) = v.as_object_mut() {
            if let Some(name) = o.remove("name") {
                o.insert("title".into(), name);
            }
            if let Some(text) = o.remove("instructions") {
                o.insert("answer".into(), text);
            }
        }
        let cells: Vec<String> = CSV_COLUMNS
            .iter()
            .map(|c| match (c, v.get(*c)) {
                (&"steps", Some(s)) | (&"attrs", Some(s)) | (&"links", Some(s)) => serde_json::to_string(s).unwrap_or_default(),
                (_, Some(Value::String(s))) => s.clone(),
                (_, Some(Value::Null)) | (_, None) => String::new(),
                (_, Some(other)) => other.to_string(),
            })
            .map(|cell| csv_cell(&cell, delimiter))
            .collect();
        out.push_str(&cells.join(&delimiter.to_string()));
        out.push_str("\r\n");
    }
    out
}

fn csv_cell(value: &str, delimiter: char) -> String {
    let guarded = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{value}") } else { value.to_string() };
    if guarded.contains([delimiter, '"', '\n', '\r']) {
        format!("\"{}\"", guarded.replace('"', "\"\""))
    } else {
        guarded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(q: &str, a: &str) -> Value {
        json!({"kind": "fact", "question": q, "answer": a})
    }

    fn req(rows: Vec<Value>, dry_run: bool, on_error: OnError) -> ImportRequest {
        ImportRequest { dry_run, on_error, rows, ..Default::default() }
    }

    #[test]
    fn a_plan_names_what_each_row_would_do() {
        let mut db = Vec::new();
        run(req(vec![fact("what is pin 4 of the ne555", "RESET"), fact("lm317 vref", "1.25 V")], false, OnError::Abort), &mut db);

        let plan = run(
            req(
                vec![
                    fact("what is pin 4 of the ne555", "RESET"),         // identical: nothing to do
                    fact("lm317 vref", "1.25 V between OUT and ADJ"),   // same question, new answer
                    fact("ne556 timers", "two"),                        // new
                    fact("ne556 timers", "two 555 timers in one chip"), // repeats the row above
                    json!({"kind": "fact", "question": "no answer"}),   // invalid
                ],
                true,
                OnError::Abort,
            ),
            &mut db,
        );
        let s: Vec<Status> = plan.rows.iter().map(|r| r.status).collect();
        assert_eq!(s, vec![Status::Unchanged, Status::Replace, Status::Duplicate, Status::New, Status::Error]);
        let codes: Vec<Option<&str>> = plan.rows.iter().map(|r| r.code).collect();
        assert_eq!(codes, vec![None, Some("replace"), Some("duplicate"), None, Some("missing_answer")]);
        assert_eq!(plan.rows[2].arg.as_deref(), Some("4"), "the duplicate names the row that wins");
        assert_eq!(db.len(), 2, "a dry run writes nothing");
        assert!(plan.rows[2].message.as_deref().unwrap().contains("row 4"));
    }

    #[test]
    fn abort_writes_nothing_and_skip_writes_the_valid_rows() {
        let rows = || vec![fact("a b", "1"), json!({"kind": "fact", "question": "broken"})];

        let mut db = Vec::new();
        let p = run(req(rows(), false, OnError::Abort), &mut db);
        assert!(p.blocked() && !p.written && db.is_empty());

        let p = run(req(rows(), false, OnError::Skip), &mut db);
        assert!(p.written && !p.blocked());
        assert_eq!(db.len(), 1);
    }

    #[test]
    fn export_then_import_is_all_unchanged_and_keeps_old_dates() {
        let mut db = Vec::new();
        run(
            req(
                vec![
                    json!({"kind": "fact", "question": "What is the NE555?", "answer": "=a timer, -5 V tolerant, \"quoted\"; with, commas\nand a newline", "last_verified": "2020-01-01"}),
                    json!({"kind": "procedure", "intent": "Flush the DNS cache", "command": "ipconfig /flushdns", "shell": "cmd", "last_verified": "2020-01-01"}),
                ],
                false,
                OnError::Abort,
            ),
            &mut db,
        );
        assert_eq!(db.len(), 2);

        // Through JSON.
        let rows: Vec<Value> = db.iter().map(export_row).collect();
        let p = run(req(rows, false, OnError::Abort), &mut db);
        assert_eq!((p.summary.unchanged, p.summary.new, p.summary.replace), (2, 0, 0), "{p:#?}");
        assert!(!p.written);
        assert!(db.iter().all(|r| r.last_verified == "2020-01-01"), "unchanged rows keep their date");

        // Through CSV, with the hostile cell, both delimiters.
        for delim in [',', ';'] {
            let refs: Vec<&Record> = db.iter().collect();
            let csv = export_csv(&refs, delim);
            assert!(csv.starts_with('\u{FEFF}'));
            assert!(csv.contains("'=a timer"), "formula-looking cells are neutralised");
            let parsed = parse_csv_for_test(csv.trim_start_matches('\u{FEFF}'), delim);
            let p = run(req(parsed, true, OnError::Abort), &mut db);
            assert_eq!(p.summary.unchanged, 2, "delimiter {delim:?}: {p:#?}");
        }
    }

    #[test]
    fn types_attrs_links_and_contexts_survive_json_and_csv() {
        let mut db = Vec::new();
        let p = run(
            req(
                vec![
                    json!({"kind": "fact", "question": "router admin ip", "answer": "192.168.0.1", "type": "network",
                           "attrs": {"vendor": "TP-Link"}, "links": ["reboot the router"]}),
                    json!({"kind": "procedure", "intent": "reboot the router", "command": "reboot", "type": "command",
                           "links": "router admin ip; router back panel"}),
                    json!({"kind": "context", "name": "Network support", "instructions": "Answer briefly. [[reboot the router]]",
                           "links": ["router admin ip"]}),
                ],
                false,
                OnError::Abort,
            ),
            &mut db,
        );
        assert_eq!(p.summary.new, 3, "{p:#?}");
        let fact = db.iter().find(|r| r.kind == Kind::Fact).unwrap();
        assert_eq!((fact.rtype.as_str(), fact.attrs["vendor"].as_str()), ("network", "TP-Link"));
        let proc_ = db.iter().find(|r| r.kind == Kind::Procedure).unwrap();
        assert_eq!(proc_.links, vec!["router admin ip".to_string(), "router back panel".to_string()]);
        assert!(!proc_.body.contains("links") && !proc_.body.contains("\"type\""), "shape fields never leak into the body");
        let ctx = db.iter().find(|r| r.rtype == crate::query::CONTEXT_TYPE).unwrap();
        assert_eq!(ctx.title, "Network support");

        let rows: Vec<Value> = db.iter().map(export_row).collect();
        assert_eq!(run(req(rows, true, OnError::Abort), &mut db).summary.unchanged, 3);
        for delim in [',', ';'] {
            let refs: Vec<&Record> = db.iter().collect();
            let parsed = parse_csv_for_test(export_csv(&refs, delim).trim_start_matches('\u{FEFF}'), delim);
            let p = run(req(parsed, true, OnError::Abort), &mut db);
            assert_eq!(p.summary.unchanged, 3, "delimiter {delim:?}: {p:#?}");
        }
    }

    fn image_doc(name: &str, bytes: &[u8]) -> Document {
        use base64::Engine as _;
        Document {
            name: name.into(),
            text: String::new(),
            collection: Some("red".into()),
            data: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            media_type: None,
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    #[test]
    fn a_preview_never_calls_the_model_and_a_real_run_describes_then_skips_the_same_image() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let describe = |bytes: Vec<u8>, prompt: String| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                assert!(prompt.contains("Spanish"));
                Ok::<_, String>(format!("Panel trasero; {} bytes.", bytes.len()))
            }
        };
        let req = |dry| ImportRequest {
            dry_run: dry,
            lang: Some("es".into()),
            vision_model: Some("fake-vision".into()),
            documents: vec![image_doc("fotos/router.jpg", b"\xff\xd8 jpeg bytes")],
            ..Default::default()
        };
        let mut db = Vec::new();

        let p = block_on(run_async(req(true), &mut db, &describe));
        assert_eq!((p.documents[0].status, p.documents[0].code), (Status::New, Some("describe")));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "a preview must cost nothing");
        assert!(db.is_empty());

        let p = block_on(run_async(req(false), &mut db, &describe));
        assert!(p.written);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let r = &db[0];
        assert_eq!((r.rtype.as_str(), r.verified_by.as_str()), ("image", "model:fake-vision"));
        assert!(r.body.starts_with("Panel trasero"));

        // A person corrects the description; the same image again leaves it be.
        db[0].body = "Router TP-Link, panel trasero (revisado).".into();
        db[0].verified_by = "human".into();
        let p = block_on(run_async(req(false), &mut db, &describe));
        assert_eq!(p.documents[0].status, Status::Unchanged);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "same bytes, no second description");
        assert_eq!(db[0].verified_by, "human");
    }

    #[test]
    fn a_failed_description_blocks_an_all_or_nothing_batch() {
        let describe = |_b: Vec<u8>, _p: String| async { Err::<String, _>("model not found".to_string()) };
        let mut db = Vec::new();
        let req = ImportRequest {
            rows: vec![fact("lm317 vref", "1.25 V")],
            documents: vec![image_doc("a.png", b"png")],
            ..Default::default()
        };
        let p = block_on(run_async(req, &mut db, describe));
        assert_eq!((p.documents[0].status, p.documents[0].code), (Status::Error, Some("vision_failed")));
        assert!(p.blocked() && !p.written);
        assert!(db.is_empty(), "the valid fact is not written either");
    }

    #[test]
    fn documents_split_like_fds_add_and_re_import_unchanged() {
        let mut db = Vec::new();
        let doc = || ImportRequest {
            documents: vec![Document { name: "notes/lm317.md".into(), text: "# LM317\nIntro.\n## Pinout\nADJ OUT IN".into(), collection: Some("electronics".into()), data: None, media_type: None }],
            ..Default::default()
        };
        let p = run(doc(), &mut db);
        assert_eq!((p.documents[0].status, p.documents[0].fragments), (Status::New, 2));
        assert!(db.iter().all(|r| r.kind == Kind::Knowledge && r.collection == "electronics"));
        let p = run(doc(), &mut db);
        assert_eq!(p.documents[0].status, Status::Unchanged);
        assert!(!p.written);
    }

    #[test]
    fn a_binary_or_foreign_document_is_an_error_not_a_crash() {
        let mut db = Vec::new();
        let p = run(
            ImportRequest {
                documents: vec![
                    Document { name: "photo.png".into(), text: "x".into(), collection: None, data: None, media_type: None },
                    Document { name: "a.md".into(), text: "a\0b".into(), collection: None, data: None, media_type: None },
                ],
                ..Default::default()
            },
            &mut db,
        );
        assert!(p.documents.iter().all(|d| d.status == Status::Error));
        assert!(p.blocked() || p.dry_run || !p.written);
        assert!(db.is_empty());
    }

    /// The real reader, not a stand-in: the round trip goes through exactly
    /// what `fds import` and the manager use.
    fn parse_csv_for_test(text: &str, delim: char) -> Vec<Value> {
        let t = crate::tabular::parse_csv(text, delim);
        crate::tabular::rows_from_table(&t[0], &t[1..], None)
    }
}
