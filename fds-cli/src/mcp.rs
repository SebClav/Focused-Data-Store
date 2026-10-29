//! MCP (SPEC.md §9): FDS's tools for an agent, over stdio or HTTP.
//!
//! One handler, `respond`, serves both transports: `fds mcp` reads JSON-RPC
//! lines on stdin, and `fds serve` answers POST /mcp (serve.rs). It speaks
//! both eras of the protocol: the stateless per-request metadata of
//! 2026-07-28, and the `initialize` handshake of the revisions before it.
//!
//! stdout carries only JSON-RPC frames; all logging goes to stderr.
//! Responses are data -- records, confidence, provenance -- and contain no
//! instructions to the model (§1).

use std::path::PathBuf;

use base64::Engine as _;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::query::{verified_list, ContextBundle, Engine, QueryResult, Scope, DEFAULT_BUDGET};
use crate::sectors::{self, View};
use crate::text::today;
use crate::{db, ops, policy, sensitive, status};

/// Newest first. 2026-07-28 is stateless; the others open with `initialize`.
pub const SUPPORTED: &[&str] = &[
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];
const MODERN: &str = "2026-07-28";
/// Offered to a legacy client that asks for a version this server lacks.
const LEGACY_DEFAULT: &str = "2025-06-18";
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const INSTRUCTIONS: &str =
    "FDS stores knowledge as records -- facts, procedures, document fragments -- \
each with its source, who verified it and when. fds_query returns records with a confidence of \
exact, strong, weak or none; it never generates text.";

/// Where an agent's own memory goes: `fds_remember` writes here and nowhere else.
pub struct Memory {
    pub path: PathBuf,
    pub sector: String,
    /// Recorded as `verified_by`, e.g. `model:sales`: written by an agent,
    /// waiting for a person to review it.
    pub writer: String,
}

/// What one agent can see and change, for one request.
pub struct Session<'a> {
    /// Its own sector first, then the sectors it reads.
    pub views: Vec<View<'a>>,
    pub memory: Option<&'a Memory>,
}

fn tools(memory: bool) -> Value {
    let mut list = vec![
        json!({
            "name": "fds_query",
            "description": "Query a FDS knowledge database. Returns verified facts, procedures, or \
        knowledge fragments with a confidence level (exact, strong, weak, none) and provenance \
        (source, who verified it, when). The database holds domain knowledge that is not in \
        training data; query it before answering questions in its domain. Accents and case do not \
        matter, section headings count, and a file name or path (`docs/brand.md`) finds the sections of that file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "The question or intent, in natural language."},
                    "collection": {"type": "string", "description": "Optional collection to restrict the search to."},
                    "budget": {"type": "integer", "description": "Maximum tokens of knowledge to return. Default 400."},
                    "with_links": {"type": "boolean", "description": "Also return the records the results link to, one hop out."},
                    "verified_by": {"type": "string", "description": "Only records verified by these, comma-separated: human, ingest, or model: for any model."}
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "fds_contexts",
            "description": "List the stored contexts: named bundles of instructions plus the records they link to. Load one with fds_context instead of re-reading its pieces one by one.",
            "inputSchema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "fds_context",
            "description": "Load a stored context in one call: its instructions and every record it links to, with a hash. If you already hold this context, pass its hash as known_hash: when nothing changed the answer is one line, not the whole bundle, so it costs almost no tokens.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "The context's name, or its id."},
                    "known_hash": {"type": "string", "description": "The hash of the version you already hold, if any."}
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "fds_check",
            "description": "Ask whether an action is allowed BEFORE you do it. Give what it touches (subject), the operation (action) and, when you are setting something, the value. The answer is allow or deny, with the rule and the person's own words for it when it denies. It is decided by comparing, not by a model, and it cannot be argued with: if it says deny, do not do it and do not look for another way. If no rule covers the action the answer says so (default_applied): that is not permission to ignore the rules of whoever runs you. A guard may enforce the same rules on the calls you make.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "subject": {"type": "string", "description": "What the action touches, e.g. fridge.chamber3.setpoint or db.prod.customers."},
                    "action": {"type": "string", "description": "The operation: set, delete, drop, read_raw..."},
                    "value": {"type": "string", "description": "The value being set, when there is one."}
                },
                "required": ["subject", "action"]
            }
        }),
        json!({
            "name": "fds_status",
            "description": "Summarise the FDS database: record counts by kind, collections, and whether it has embeddings. Use it to learn what domain the database covers.",
            "inputSchema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "fds_journal",
            "description": "Read-only: read the project's change journal, the account the agents left of what they did and why (written with fds_log). Use it to review work you did not see: it returns the entries as a numbered timeline, oldest first, and under it each entry's detail (why, files, branch, commit, who wrote it and whether a person has reviewed it yet). All filters are optional. It never writes anything.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "How many of the newest entries to return. Default 50."},
                    "agent": {"type": "string", "description": "Only entries written by this agent."},
                    "role": {"type": "string", "description": "Only entries with this role: reasoning, decision, execution..."},
                    "branch": {"type": "string", "description": "Only entries on this git branch."},
                    "since": {"type": "string", "description": "Only entries from this day on, YYYY-MM-DD."},
                    "timeline": {"type": "boolean", "description": "true (default): numbered timeline, oldest first, with each entry's detail. false: the plain list, newest first."}
                }
            }
        }),
    ];
    if memory {
        list.push(json!({
            "name": "fds_remember",
            "description": "Save something worth keeping in your own memory: a question phrased the way someone would ask it, and its answer. It is stored as written by you until a person reviews it, and only you and the people who manage this database see it. Saving the same question again replaces your earlier answer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "How someone would ask for this."},
                    "answer": {"type": "string", "description": "What to remember."},
                    "collection": {"type": "string", "description": "Optional grouping. Default: memory."}
                },
                "required": ["question", "answer"]
            }
        }));
        list.push(json!({
            "name": "fds_log",
            "description": "When you finish a piece of work on a project, write down what you did, for the developer who will read this code later and was not in your session. One entry per piece of work: say what changed and why, name the files, and give the branch and commit if you know them. Write for a person who has never seen this conversation: what the code does now that it did not do before, the decision you took and the alternative you dropped, anything fragile. Do not paste code or logs. A person reviews your entries; they appear in the project's change journal.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": {"type": "string", "description": "One line, under 200 characters: what changed."},
                    "why": {"type": "string", "description": "Why, how, and what the reader should watch out for."},
                    "files": {"type": "array", "items": {"type": "string"}, "description": "Paths of the files you changed."},
                    "branch": {"type": "string", "description": "The git branch you worked on, if you know it."},
                    "commit": {"type": "string", "description": "The commit that contains the change, if you know it."},
                    "role": {"type": "string", "description": "The part you play in this work, if you were given one: reasoning, decision, execution..."},
                    "status": {"type": "string", "description": "Where this step stands: running, finished, blocked..."},
                    "to": {"type": "string", "description": "Who you handed this to, when the step is a delegation."}
                },
                "required": ["summary"]
            }
        }));
        list.push(json!({
            "name": "fds_forget",
            "description": "Remove a record you saved earlier with fds_remember, by its id (fds_query shows ids). Records a person verified cannot be removed this way.",
            "inputSchema": {
                "type": "object",
                "properties": {"id": {"type": "string", "description": "The record's id, or its first 6+ characters."}},
                "required": ["id"]
            }
        }));
    }
    Value::Array(list)
}

fn render(r: &QueryResult) -> String {
    let mut out = format!(
        "confidence: {}\nrecords: {}\n",
        r.confidence.as_str(),
        r.records.len()
    );
    for h in &r.records {
        let sector = h
            .sector
            .as_deref()
            .map(|s| format!(" | sector: {s}"))
            .unwrap_or_default();
        let kind = if h.rtype.is_empty() {
            h.kind.as_str().to_string()
        } else {
            format!("{}/{}", h.kind.as_str(), h.rtype)
        };
        out.push_str(&format!(
            "\n--- [{kind}] {} ---\nid: {}{sector} | source: {} | verified_by: {} | last_verified: {} | match: {} | score: {}\n",
            h.question.as_deref().unwrap_or(&h.title),
            &h.id[..h.id.len().min(12)],
            h.source,
            h.verified_by,
            h.last_verified,
            h.match_kind,
            h.score
        ));
        out.push_str(&h.body);
        out.push('\n');
        if !h.attrs.is_empty() {
            let attrs: Vec<String> = h.attrs.iter().map(|(k, v)| format!("{k}={v}")).collect();
            out.push_str(&format!("attrs: {}\n", attrs.join(" | ")));
        }
    }
    out
}

fn render_context(b: &ContextBundle) -> String {
    let mut out = format!(
        "context: {}\nhash: {}\ntokens: {}\nrecords: {}\n\n## instructions\n{}\n",
        b.name,
        b.hash,
        b.tokens,
        b.records.len(),
        b.instructions
    );
    for h in &b.records {
        let kind = if h.rtype.is_empty() {
            h.kind.as_str().to_string()
        } else {
            format!("{}/{}", h.kind.as_str(), h.rtype)
        };
        out.push_str(&format!(
            "\n--- [{kind}] {} ---\nsource: {} | verified_by: {} | last_verified: {}\n{}\n",
            h.question.as_deref().unwrap_or(&h.title),
            h.source,
            h.verified_by,
            h.last_verified,
            h.body
        ));
    }
    if !b.missing.is_empty() {
        out.push_str(&format!(
            "\nlinks to records that do not exist yet: {}\n",
            b.missing.join(", ")
        ));
    }
    out
}

fn text(t: String, is_error: bool) -> Value {
    if is_error {
        json!({"content": [{"type": "text", "text": t}], "isError": true})
    } else {
        json!({"content": [{"type": "text", "text": t}]})
    }
}

fn arg<'v>(args: &'v Value, key: &str) -> Option<&'v str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

async fn call(s: &Session<'_>, name: &str, args: &Value) -> Value {
    match name {
        "fds_status" => {
            if s.views.len() == 1 {
                let v = &s.views[0];
                let sum =
                    status::summary(&PathBuf::from(v.name), &v.engine.meta, &v.engine.records);
                return text(
                    serde_json::to_string_pretty(&sum).unwrap_or_default(),
                    false,
                );
            }
            let all: Vec<Value> = s
                .views
                .iter()
                .map(|v| json!({"sector": v.name, "status": status::summary(&PathBuf::from(v.name), &v.engine.meta, &v.engine.records)}))
                .collect();
            text(
                serde_json::to_string_pretty(&all).unwrap_or_default(),
                false,
            )
        }
        "fds_query" => {
            let Some(q) = arg(args, "query") else {
                return text("missing required parameter: query".into(), true);
            };
            let collection = arg(args, "collection");
            let budget = args
                .get("budget")
                .and_then(|v| v.as_u64())
                .map(|b| b as u32)
                .unwrap_or(DEFAULT_BUDGET);
            let verified = verified_list(arg(args, "verified_by"));
            let scope = Scope {
                collection,
                verified_by: &verified,
            };
            let links = args
                .get("with_links")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let result = sectors::query(&s.views, q, &scope, budget, links).await;
            text(render(&result), false)
        }
        "fds_journal" => {
            let since = arg(args, "since").map(String::from);
            if let Some(d) = &since {
                let b = d.as_bytes();
                if b.len() != 10
                    || !b.iter().enumerate().all(|(i, c)| {
                        if i == 4 || i == 7 {
                            *c == b'-'
                        } else {
                            c.is_ascii_digit()
                        }
                    })
                {
                    return text(format!("since must be a day, YYYY-MM-DD; got `{d}`"), true);
                }
            }
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize)
                .unwrap_or(50)
                .clamp(1, 500);
            let timeline = args
                .get("timeline")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let filter = crate::journal::Filter {
                branch: arg(args, "branch").map(String::from),
                agent: arg(args, "agent").map(String::from),
                role: arg(args, "role").map(String::from),
                since,
                limit: Some(limit),
            };
            // The engines hold what this session may read: sensitive records are
            // already stand-ins (§11.4), so the journal cannot show more than a query.
            let views: Vec<(&str, &[fds::v4::Record])> = s
                .views
                .iter()
                .map(|v| (v.name, v.engine.records.as_slice()))
                .collect();
            text(crate::journal::read_text(&views, &filter, timeline), false)
        }
        "fds_check" => {
            let (Some(subject), Some(action)) = (arg(args, "subject"), arg(args, "action")) else {
                return text(
                    "missing required parameters: subject and action".into(),
                    true,
                );
            };
            let value = match args.get("value") {
                Some(Value::String(v)) => Some(v.trim().to_string()).filter(|v| !v.is_empty()),
                Some(Value::Number(n)) => Some(n.to_string()),
                Some(Value::Bool(b)) => Some(b.to_string()),
                _ => None,
            };
            let policies: Vec<policy::Policy> = s
                .views
                .iter()
                .flat_map(|v| policy::policies(&v.engine.records))
                .collect();
            let d = policy::check(
                &policies,
                subject,
                action,
                value.as_deref(),
                policy::Fallback::Allow,
            );
            let body = serde_json::to_string_pretty(&d).unwrap_or_default();
            text(format!("{}\n\n{body}", d.summary()), false)
        }
        "fds_contexts" => {
            let many = s.views.len() > 1;
            let list: Vec<String> = s
                .views
                .iter()
                .flat_map(|v| {
                    v.engine.contexts().into_iter().map(move |i| {
                        let b = v.engine.context(i);
                        let at = if many {
                            format!(" [{}]", v.name)
                        } else {
                            String::new()
                        };
                        format!(
                            "- {}{at} ({} records, {} tokens, hash {})",
                            b.name,
                            b.records.len(),
                            b.tokens,
                            b.hash
                        )
                    })
                })
                .collect();
            text(
                if list.is_empty() {
                    "no contexts stored".into()
                } else {
                    list.join("\n")
                },
                false,
            )
        }
        "fds_context" => {
            let Some(name) = arg(args, "name") else {
                return text("missing required parameter: name".into(), true);
            };
            let Some((v, i)) = s
                .views
                .iter()
                .find_map(|v| v.engine.find_context(name).map(|i| (v, i)))
            else {
                return text(
                    format!("no context named `{name}`; fds_contexts lists them"),
                    true,
                );
            };
            let b = v.engine.context(i);
            if arg(args, "known_hash") == Some(b.hash.as_str()) {
                return text(
                    format!(
                        "context {} unchanged (hash {}); keep using the copy you hold",
                        b.name, b.hash
                    ),
                    false,
                );
            }
            text(render_context(&b), false)
        }
        "fds_remember" => {
            let Some(m) = s.memory else {
                return text(
                    "this connection is read-only: it has no memory sector to write to".into(),
                    true,
                );
            };
            let (Some(question), Some(answer)) = (arg(args, "question"), arg(args, "answer"))
            else {
                return text(
                    "missing required parameters: question and answer".into(),
                    true,
                );
            };
            let collection = arg(args, "collection").unwrap_or("memory");
            match remember(m, question, answer, collection) {
                Ok(id) => text(
                    format!(
                        "saved {} in sector {}, as verified_by {}",
                        &id[..id.len().min(12)],
                        m.sector,
                        m.writer
                    ),
                    false,
                ),
                Err(e) => text(e, true),
            }
        }
        "fds_log" => {
            let Some(m) = s.memory else {
                return text(
                    "this connection is read-only: it has no memory sector to write to".into(),
                    true,
                );
            };
            let Some(summary) = arg(args, "summary") else {
                return text("missing required parameter: summary".into(), true);
            };
            let files: Vec<String> = args
                .get("files")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let at = crate::text::now();
            let draft = crate::journal::Draft {
                summary,
                why: arg(args, "why").unwrap_or(""),
                files: &files,
                branch: arg(args, "branch").unwrap_or(""),
                commit: arg(args, "commit").unwrap_or(""),
                agent: m.writer.strip_prefix("model:").unwrap_or(&m.writer),
                at: &at,
                role: arg(args, "role").unwrap_or(""),
                status: arg(args, "status").unwrap_or(""),
                to: arg(args, "to").unwrap_or(""),
            };
            let saved = crate::journal::record(&draft, &m.writer)
                .and_then(|r| crate::journal::append(&m.path, r));
            match saved {
                Ok(id) => text(format!("logged {} in sector {}, as verified_by {}; a person can read it with `fds journal`", &id[..id.len().min(12)], m.sector, m.writer), false),
                Err(e) => text(e, true),
            }
        }
        "fds_forget" => {
            let Some(m) = s.memory else {
                return text(
                    "this connection is read-only: it has no memory sector to change".into(),
                    true,
                );
            };
            let Some(id) = arg(args, "id") else {
                return text("missing required parameter: id".into(), true);
            };
            match forget(m, id) {
                Ok(id) => text(
                    format!(
                        "removed {} from sector {}",
                        &id[..id.len().min(12)],
                        m.sector
                    ),
                    false,
                ),
                Err(e) => text(e, true),
            }
        }
        other => text(format!("unknown tool: {other}"), true),
    }
}

fn remember(m: &Memory, question: &str, answer: &str, collection: &str) -> Result<String, String> {
    let _lock = db::lock(&m.path)?;
    let (mut meta, mut records) = db::load(&m.path)?;
    let source = format!("agent:{}", m.sector);
    let id = ops::upsert_fact(
        &mut records,
        question,
        answer,
        collection,
        &source,
        &m.writer,
        &today(),
    )?;
    db::save(&m.path, &mut meta, &records)?;
    Ok(id)
}

/// An agent may remove only what an agent wrote: a record a person verified
/// in its sector stays, whatever the agent asks.
fn forget(m: &Memory, id: &str) -> Result<String, String> {
    let _lock = db::lock(&m.path)?;
    let (mut meta, mut records) = db::load(&m.path)?;
    let i = db::resolve_id(&records, id)?;
    if !records[i].verified_by.starts_with("model:") {
        return Err(format!(
            "record {} is verified by {}; only records an agent wrote can be removed this way",
            &records[i].id[..12.min(records[i].id.len())],
            records[i].verified_by
        ));
    }
    let removed = records.remove(i).id;
    db::save(&m.path, &mut meta, &records)?;
    Ok(removed)
}

fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut e = json!({"code": code, "message": message});
    if let Some(d) = data {
        e["data"] = d;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": e})
}

fn server_info() -> Value {
    json!({"name": "fds", "version": env!("CARGO_PKG_VERSION")})
}

/// The version a modern (2026-07-28 and later) request declares in its `_meta`;
/// None for a legacy request, which negotiated its version in `initialize`.
pub fn requested_version(req: &Value) -> Option<String> {
    req.pointer("/params/_meta")
        .and_then(|m| m.get(META_VERSION))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Answers one JSON-RPC message. None for a notification, which is never answered.
pub async fn respond(req: &Value, s: &Session<'_>) -> Option<Value> {
    let id = req.get("id").cloned()?;
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));
    let modern = requested_version(req);
    if let Some(v) = &modern {
        if !SUPPORTED.contains(&v.as_str()) {
            return Some(error(
                id,
                -32022,
                "Unsupported protocol version",
                Some(json!({"supported": SUPPORTED, "requested": v})),
            ));
        }
    }
    let mut result = match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(LEGACY_DEFAULT);
            let version = if asked != MODERN && SUPPORTED.contains(&asked) {
                asked
            } else {
                LEGACY_DEFAULT
            };
            json!({"protocolVersion": version, "capabilities": {"tools": {}}, "serverInfo": server_info(), "instructions": INSTRUCTIONS})
        }
        "server/discover" => json!({
            "resultType": "complete",
            "supportedVersions": SUPPORTED,
            "capabilities": {"tools": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo": server_info()},
            "instructions": INSTRUCTIONS
        }),
        "tools/list" => json!({"tools": tools(s.memory.is_some())}),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            call(s, name, &args).await
        }
        "ping" => json!({}),
        other => {
            return Some(error(
                id,
                -32601,
                &format!("method not found: {other}"),
                None,
            ))
        }
    };
    if modern.is_some() && result.get("resultType").is_none() {
        result["resultType"] = json!("complete");
    }
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// A header value as the 2026-07-28 transport writes it: plain when it is
/// printable ASCII without edge spaces, else `=?base64?…?=`.
pub fn header_value(v: &str) -> String {
    let plain = !v.is_empty()
        && v.trim() == v
        && v.bytes().all(|b| (0x20..0x7f).contains(&b))
        && !(v.starts_with("=?base64?") && v.ends_with("?="));
    if plain {
        v.to_string()
    } else {
        format!(
            "=?base64?{}?=",
            base64::engine::general_purpose::STANDARD.encode(v.as_bytes())
        )
    }
}

fn decode_header(v: &str) -> Option<String> {
    match v
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    {
        Some(b) => base64::engine::general_purpose::STANDARD
            .decode(b)
            .ok()
            .and_then(|d| String::from_utf8(d).ok()),
        None => Some(v.to_string()),
    }
}

/// The 2026-07-28 rule that headers mirror the body: a router that trusts
/// the header must not be told one thing while the server does another.
/// Legacy requests carry none of these headers and are not checked.
pub fn check_headers(req: &Value, header: impl Fn(&str) -> Option<String>) -> Result<(), String> {
    let Some(version) = requested_version(req) else {
        return Ok(());
    };
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    match header("mcp-protocol-version") {
        Some(h) if h == version => {}
        Some(h) => {
            return Err(format!(
                "MCP-Protocol-Version `{h}` does not match the body's `{version}`"
            ))
        }
        None => return Err("missing MCP-Protocol-Version header".into()),
    }
    match header("mcp-method") {
        Some(h) if h == method => {}
        Some(h) => {
            return Err(format!(
                "Mcp-Method `{h}` does not match the body's `{method}`"
            ))
        }
        None => return Err("missing Mcp-Method header".into()),
    }
    if method == "tools/call" {
        let name = req
            .pointer("/params/name")
            .and_then(|n| n.as_str())
            .unwrap_or("");
        match header("mcp-name").and_then(|h| decode_header(&h)) {
            Some(h) if h == name => {}
            Some(h) => return Err(format!("Mcp-Name `{h}` does not match the body's `{name}`")),
            None => return Err("missing Mcp-Name header".into()),
        }
    }
    Ok(())
}

pub fn header_mismatch(id: Option<Value>, message: &str) -> Value {
    error(
        id.unwrap_or(Value::Null),
        -32020,
        &format!("Header mismatch: {message}"),
        None,
    )
}

struct Loaded {
    name: String,
    path: PathBuf,
    engine: Engine,
    stamp: Option<db::Stamp>,
}

/// `fds mcp` over stdio. `files` are the sectors this agent sees, its own
/// first; `memory`, when given, is where `fds_remember` writes.
pub async fn run(
    files: Vec<(String, PathBuf)>,
    memory: Option<Memory>,
    clearance: Vec<String>,
) -> Result<(), String> {
    let mut loaded = Vec::with_capacity(files.len());
    for (name, path) in files {
        let (meta, records) = db::load(&path)?;
        let records = sensitive::redact(&records, &clearance);
        loaded.push(Loaded {
            stamp: db::stamp(&path),
            engine: Engine::new(meta, records),
            name,
            path,
        });
    }
    let total: usize = loaded.iter().map(|l| l.engine.records.len()).sum();
    let names: Vec<&str> = loaded.iter().map(|l| l.name.as_str()).collect();
    eprintln!(
        "fds mcp: {} ({total} records){}",
        names.join(" + "),
        memory
            .as_ref()
            .map(|m| format!(", remembers into `{}`", m.sector))
            .unwrap_or_default()
    );

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(line) else {
            eprintln!("fds mcp: ignoring malformed JSON");
            continue;
        };
        // An agent's session can outlive many writes -- its own included;
        // never answer from a copy older than the file.
        for l in loaded.iter_mut() {
            let now = db::stamp(&l.path);
            if now != l.stamp {
                if let Ok((meta, records)) = db::load(&l.path) {
                    l.engine = Engine::new(meta, sensitive::redact(&records, &clearance));
                    l.stamp = now;
                }
            }
        }
        let session = Session {
            views: loaded
                .iter()
                .map(|l| View {
                    name: &l.name,
                    engine: &l.engine,
                })
                .collect(),
            memory: memory.as_ref(),
        };
        let Some(resp) = respond(&req, &session).await else {
            continue;
        };
        let mut payload = serde_json::to_string(&resp).unwrap_or_default();
        payload.push('\n');
        if stdout.write_all(payload.as_bytes()).await.is_err() {
            break;
        }
        let _ = stdout.flush().await;
    }
    Ok(())
}

/// `fds mcp --connect URL`: a stdio client's bridge to a running
/// `fds serve`. It loads no data; every message is forwarded to the one
/// server that holds the files. The agent key comes from `FDS_KEY`, never
/// from the command line, where other processes could read it.
pub async fn connect(url: &str) -> Result<(), String> {
    let key = std::env::var("FDS_KEY")
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty());
    let client = reqwest::Client::new();
    eprintln!(
        "fds mcp: forwarding to {url}{}",
        if key.is_some() {
            " with the key in FDS_KEY"
        } else {
            " without a key (read-only)"
        }
    );

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(line) else {
            eprintln!("fds mcp: ignoring malformed JSON");
            continue;
        };
        let id = req.get("id").cloned();
        let mut rb = client
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Some(k) = &key {
            rb = rb.bearer_auth(k);
        }
        if let Some(v) = requested_version(&req) {
            let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
            rb = rb
                .header("MCP-Protocol-Version", v)
                .header("Mcp-Method", method);
            if let Some(name) = req.pointer("/params/name").and_then(|n| n.as_str()) {
                rb = rb.header("Mcp-Name", header_value(name));
            }
        }
        let reply = match rb.body(line.to_string()).send().await {
            Ok(res) => {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                match serde_json::from_str::<Value>(&body) {
                    Ok(v) if v.get("jsonrpc").is_some() => Some(v),
                    _ => id.clone().map(|id| {
                        error(
                            id,
                            -32603,
                            &format!("fds serve answered {status}: {}", body.trim()),
                            None,
                        )
                    }),
                }
            }
            Err(e) => id.clone().map(|id| {
                error(
                    id,
                    -32603,
                    &format!("cannot reach fds serve at {url}: {e}"),
                    None,
                )
            }),
        };
        // A notification gets no answer, whatever the server sent back.
        let (Some(_), Some(reply)) = (id, reply) else {
            continue;
        };
        let mut payload = serde_json::to_string(&reply).unwrap_or_default();
        payload.push('\n');
        if stdout.write_all(payload.as_bytes()).await.is_err() {
            break;
        }
        let _ = stdout.flush().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fds::v4::DbMeta;

    fn engine() -> Engine {
        let mut records = Vec::new();
        ops::upsert_fact(
            &mut records,
            "office wifi password",
            "ask reception",
            "default",
            "test",
            "human",
            "2026-10-12",
        )
        .unwrap();
        Engine::new(DbMeta::default(), records)
    }

    /// An image's file and alt text must reach the agent, not only the JSON.
    #[tokio::test]
    async fn a_records_type_and_attributes_are_rendered_for_the_agent() {
        let mut records = Vec::new();
        let id = ops::upsert_fact(
            &mut records,
            "foto de la fachada",
            "Fachada con toldo naranja",
            "imagenes",
            "img/fachada.jpg",
            "human",
            "2026-10-15",
        )
        .unwrap();
        let r = records.iter_mut().find(|r| r.id == id).unwrap();
        r.rtype = "image".into();
        r.attrs.insert("file".into(), "img/fachada.jpg".into());
        r.attrs.insert("alt".into(), "Fachada con toldo".into());
        let e = Engine::new(DbMeta::default(), records);
        let s = Session {
            views: vec![View {
                name: "kb",
                engine: &e,
            }],
            memory: None,
        };
        let r = respond(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "fds_query", "arguments": {"query": "foto de la fachada"}}}),
            &s,
        )
        .await
        .unwrap();
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("[fact/image]"), "{text}");
        assert!(
            text.contains("alt=Fachada con toldo") && text.contains("file=img/fachada.jpg"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn legacy_and_modern_clients_are_both_served() {
        let e = engine();
        let s = Session {
            views: vec![View {
                name: "kb",
                engine: &e,
            }],
            memory: None,
        };
        let legacy = respond(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}), &s)
            .await
            .unwrap();
        assert_eq!(legacy["result"]["protocolVersion"], "2025-06-18");
        let meta = json!({META_VERSION: MODERN});
        let modern = respond(
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": meta}}),
            &s,
        )
        .await
        .unwrap();
        assert_eq!(modern["result"]["resultType"], "complete");
        assert_eq!(
            modern["result"]["tools"].as_array().unwrap().len(),
            6,
            "read-only: no memory tools"
        );
        let unknown = respond(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {"_meta": {META_VERSION: "1900-01-01"}}}), &s)
            .await
            .unwrap();
        assert_eq!(unknown["error"]["code"], -32022);
        assert!(respond(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            &s
        )
        .await
        .is_none());
    }

    #[test]
    fn headers_must_match_the_body() {
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": "fds_query", "_meta": {META_VERSION: MODERN}}});
        let ok = |k: &str| match k {
            "mcp-protocol-version" => Some(MODERN.to_string()),
            "mcp-method" => Some("tools/call".to_string()),
            "mcp-name" => Some(header_value("fds_query")),
            _ => None,
        };
        assert!(check_headers(&req, ok).is_ok());
        let lying = |k: &str| match k {
            "mcp-name" => Some("fds_forget".to_string()),
            other => ok(other),
        };
        assert!(check_headers(&req, lying).is_err());
        assert_eq!(
            decode_header(&header_value("año ñandú")).as_deref(),
            Some("año ñandú")
        );
    }

    #[tokio::test]
    async fn an_agent_remembers_into_its_own_file_and_cannot_forget_what_a_person_verified() {
        let dir = std::env::temp_dir().join(format!("fds-mem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sales.fds");
        let mut records = Vec::new();
        ops::upsert_fact(
            &mut records,
            "refund window",
            "30 days",
            "default",
            "manual",
            "human",
            "2026-10-12",
        )
        .unwrap();
        db::save(&path, &mut DbMeta::default(), &records).unwrap();
        let m = Memory {
            path: path.clone(),
            sector: "sales".into(),
            writer: "model:sales".into(),
        };

        let id = remember(&m, "client acme prefers", "invoices in USD", "memory").unwrap();
        let (_, records) = db::load(&path).unwrap();
        let r = records.iter().find(|r| r.id == id).unwrap();
        assert_eq!(r.verified_by, "model:sales");
        assert!(forget(&m, &id).is_ok());

        let human = records
            .iter()
            .find(|r| r.verified_by == "human")
            .unwrap()
            .id
            .clone();
        assert!(forget(&m, &human).is_err(), "a person's record stays");
        assert_eq!(db::load(&path).unwrap().1.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn journal_records(rows: &[(&str, &str, &str, &str, &str)]) -> Vec<fds::v4::Record> {
        // (summary, agent, at, role, to): all written through the same path an agent uses.
        rows.iter()
            .enumerate()
            .map(|(i, (summary, agent, at, role, to))| {
                let d = crate::journal::Draft {
                    summary,
                    agent,
                    at,
                    role,
                    to,
                    why: "because reasons",
                    branch: "feat/x",
                    ..Default::default()
                };
                let mut r = crate::journal::record(
                    &d,
                    &if agent.is_empty() {
                        "human".to_string()
                    } else {
                        format!("model:{agent}")
                    },
                )
                .unwrap();
                r.attrs.insert("seq".into(), (i + 1).to_string());
                r
            })
            .collect()
    }

    async fn journal_text(s: &Session<'_>, args: Value) -> (String, bool) {
        let r = respond(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "fds_journal", "arguments": args}}), s)
            .await
            .unwrap();
        (
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string(),
            r["result"]["isError"] == true,
        )
    }

    #[tokio::test]
    async fn a_reviewer_reads_the_journal_without_memory_and_it_only_reads() {
        let recs = journal_records(&[
            ("problem recognized", "", "2026-10-23T10:00:00Z", "me", ""),
            (
                "technical decision delegated",
                "claude-r",
                "2026-10-23T10:00:01Z",
                "reasoning",
                "decision agent",
            ),
            (
                "task 1",
                "claude-e1",
                "2026-10-23T10:00:02Z",
                "execution",
                "",
            ),
            (
                "task 2",
                "claude-e2",
                "2026-10-24T09:00:00Z",
                "execution",
                "",
            ),
        ]);
        let before = recs.clone();
        let e = Engine::new(DbMeta::default(), recs);
        let s = Session {
            views: vec![View {
                name: "kb",
                engine: &e,
            }],
            memory: None,
        };

        let listed = respond(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            &s,
        )
        .await
        .unwrap();
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"fds_journal") && !names.contains(&"fds_log"),
            "{names:?}"
        );

        // The timeline, oldest first, as `fds journal show --timeline` prints it, then the detail.
        let (t, err) = journal_text(&s, json!({})).await;
        assert!(!err, "{t}");
        let want = "#01 [me] problem recognized\n#02 [reasoning] technical decision delegated \u{2192} decision agent\n#03 [execution] task 1\n#04 [execution] task 2\n";
        assert!(t.starts_with(want), "{t}");
        assert!(t.contains("#02 technical decision delegated\n    by agent `claude-r`, not yet reviewed | 2026-10-23 10:00 | branch feat/x\n    why: because reasons"), "{t}");
        assert!(
            t.contains("#01 problem recognized\n    by a person |"),
            "{t}"
        );

        // The plain list is the one `fds journal show` prints: newest first.
        let (t, _) = journal_text(&s, json!({"timeline": false})).await;
        assert!(
            t.find("task 2").unwrap() < t.find("problem recognized").unwrap(),
            "{t}"
        );
        assert!(!t.contains("#01"), "{t}");

        // limit keeps the newest and still reads oldest first; agent and role filter.
        let (t, _) = journal_text(&s, json!({"limit": 2})).await;
        assert!(
            t.starts_with("#01 [execution] task 1\n#02 [execution] task 2\n"),
            "{t}"
        );
        let (t, _) = journal_text(&s, json!({"role": "execution"})).await;
        assert!(
            t.starts_with("#01 [execution] task 1\n#02 [execution] task 2\n\n"),
            "{t}"
        );
        let (t, _) = journal_text(&s, json!({"agent": "claude-r"})).await;
        assert!(
            t.starts_with("#01 [reasoning] technical decision delegated") && !t.contains("task"),
            "{t}"
        );
        let (t, _) = journal_text(&s, json!({"since": "2026-10-24"})).await;
        assert!(t.contains("task 2") && !t.contains("task 1"), "{t}");
        let (t, err) = journal_text(&s, json!({"since": "yesterday"})).await;
        assert!(err && t.contains("YYYY-MM-DD"), "{t}");

        assert_eq!(e.records, before, "reading changes nothing");
    }

    #[tokio::test]
    async fn an_empty_journal_says_so_and_is_not_an_error() {
        let e = Engine::new(DbMeta::default(), Vec::new());
        let s = Session {
            views: vec![View {
                name: "kb",
                engine: &e,
            }],
            memory: None,
        };
        let (t, err) = journal_text(&s, json!({})).await;
        assert!(
            !err && t.trim() == "el registro est\u{e1} vac\u{ed}o",
            "{t}"
        );
        let (t, err) = journal_text(&s, json!({"timeline": false, "agent": "nobody"})).await;
        assert!(
            !err && t.starts_with("el registro est\u{e1} vac\u{ed}o"),
            "{t}"
        );
    }

    #[tokio::test]
    async fn the_journal_reads_every_sector_and_hides_what_the_agent_may_not_read() {
        let shared =
            journal_records(&[("shared change", "claude-s", "2026-10-23T08:00:00Z", "", "")]);
        let mut mine =
            journal_records(&[("own change", "claude-o", "2026-10-23T09:00:00Z", "", "")]);
        // A sensitive entry: the stand-in must reach the reader, never the text.
        let mut secret = journal_records(&[(
            "Rotated the vault key",
            "claude-o",
            "2026-10-23T09:30:00Z",
            "",
            "",
        )])
        .remove(0);
        secret.body = "new key is hunter2, stored in /etc/vault".into();
        secret
            .attrs
            .insert("files".into(), "secrets/vault.env".into());
        secret.attrs.insert("sensitivity".into(), "secret".into());
        secret
            .attrs
            .insert("derived".into(), "a credential was rotated".into());
        mine.push(secret);

        let view = |records: &[fds::v4::Record], clearance: &[String]| {
            Engine::new(
                DbMeta::default(),
                crate::sensitive::redact(records, clearance),
            )
        };
        let (shared_e, mine_e) = (view(&shared, &[]), view(&mine, &[]));
        let s = Session {
            views: vec![
                View {
                    name: "ops",
                    engine: &mine_e,
                },
                View {
                    name: "shared",
                    engine: &shared_e,
                },
            ],
            memory: None,
        };
        let (t, _) = journal_text(&s, json!({})).await;
        assert!(
            t.contains("shared change")
                && t.contains("own change")
                && t.contains("Rotated the vault key"),
            "{t}"
        );
        assert!(
            t.contains("sector shared") && t.contains("sector ops"),
            "every entry carries its sector: {t}"
        );
        assert!(
            t.contains("a credential was rotated") && t.contains("by a protected record"),
            "{t}"
        );
        for leak in [
            "hunter2",
            "/etc/vault",
            "secrets/vault.env",
            "claude-o, not yet",
        ] {
            assert!(!t.contains(leak), "`{leak}` leaked:\n{t}");
        }
        let (plain, _) = journal_text(&s, json!({"timeline": false})).await;
        assert!(
            !plain.contains("hunter2")
                && !plain.contains("vault.env")
                && plain.contains("a credential was rotated"),
            "{plain}"
        );

        // A sector cleared for `secret` reads it in full.
        let cleared = view(&mine, &["secret".to_string()]);
        let s = Session {
            views: vec![View {
                name: "ops",
                engine: &cleared,
            }],
            memory: None,
        };
        let (t, _) = journal_text(&s, json!({})).await;
        assert!(
            t.contains("hunter2")
                && t.contains("secrets/vault.env")
                && t.contains("agent `claude-o`"),
            "{t}"
        );
    }

    #[tokio::test]
    async fn an_agent_writes_what_it_did_and_a_developer_can_read_it() {
        let dir = std::env::temp_dir().join(format!("fds-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("backend.fds");
        db::save(&path, &mut DbMeta::default(), &[]).unwrap();
        let m = Memory {
            path: path.clone(),
            sector: "backend".into(),
            writer: "model:backend".into(),
        };
        let e = Engine::new(DbMeta::default(), Vec::new());
        let with = Session {
            views: vec![View {
                name: "backend",
                engine: &e,
            }],
            memory: Some(&m),
        };
        let call = |args: Value| json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "fds_log", "arguments": args}});

        let listed = respond(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            &with,
        )
        .await
        .unwrap();
        assert!(listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "fds_log"));

        let r = respond(
            &call(json!({
                "summary": "Add a rate limit to /login",
                "why": "Five failures in a minute lock the address for ten.",
                "files": ["src/auth.rs", "src/auth.rs", "tests/auth.rs"],
                "branch": "feat/login", "commit": "a1b2c3d"
            })),
            &with,
        )
        .await
        .unwrap();
        assert!(r["result"]["isError"].is_null(), "{r}");
        let (_, records) = db::load(&path).unwrap();
        let entries = crate::journal::entries(&records, "", &Default::default());
        assert_eq!(entries.len(), 1);
        let en = &entries[0];
        assert_eq!(
            (
                en.agent.as_str(),
                en.branch.as_str(),
                en.commit.as_str(),
                en.reviewed
            ),
            ("backend", "feat/login", "a1b2c3d", false)
        );
        assert_eq!(en.files, ["src/auth.rs", "tests/auth.rs"]);
        assert_eq!(
            records[0].verified_by, "model:backend",
            "an agent's entry waits for a person"
        );

        // The summary is required; a connection without memory cannot log.
        let bad = respond(&call(json!({"why": "no summary"})), &with)
            .await
            .unwrap();
        assert_eq!(bad["result"]["isError"], true);
        let read_only = Session {
            views: vec![View {
                name: "backend",
                engine: &e,
            }],
            memory: None,
        };
        let denied = respond(&call(json!({"summary": "x"})), &read_only)
            .await
            .unwrap();
        assert_eq!(denied["result"]["isError"], true);
        std::fs::remove_dir_all(&dir).ok();
    }
}
