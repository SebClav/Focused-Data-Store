//! `fds serve` (SPEC.md §8): the query API and the database manager, over
//! HTTP. Returns records, never generated text.
//!
//! The manager can change the database, and a local server that writes is a
//! classic target: any web page the user visits can fire requests at
//! 127.0.0.1. Three things stand in the way:
//!
//! 1. It listens on the loopback interface only.
//! 2. Every request must carry a loopback `Host` header. This stops DNS
//!    rebinding, where an attacker's domain resolves to 127.0.0.1 and so
//!    becomes "same origin" in the browser.
//! 3. Every write must carry `x-fds-token`, a secret minted at startup and
//!    handed only to the page this server itself serves. A foreign page can
//!    neither read that page (no CORS) nor guess the token.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use fds::v4::{DbMeta, Kind, Record};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::query::{verified_list, Engine, Scope, DEFAULT_BUDGET};
use crate::sectors::{self, View};
use crate::text::today;
use crate::{db, etl, mcp, ollama, ops, policy, sensitive, status};

const UI: &str = include_str!("ui/index.html");
const FONT_VT323: &[u8] = include_bytes!("ui/vt323.woff2");
const TOKEN_HEADER: &str = "x-fds-token";
const MAX_LIST: usize = 1000;
/// Imports carry whole files; the 2 MB default would refuse a modest CSV.
const MAX_IMPORT_BYTES: usize = 32 * 1024 * 1024;

pub struct AppState {
    engine: RwLock<Engine>,
    /// The file as it was when `engine` was built; see `current`.
    stamp: std::sync::Mutex<Option<db::Stamp>>,
    path: PathBuf,
    token: String,
    port: u16,
    /// Where this database's manager lives: "" alone, `/s/<sector>` with sectors.
    base: String,
}

/// What `/mcp` answers from: every database this server loaded, and how a
/// caller is identified. With one database, its manager token unlocks the
/// memory tools; with sectors, an agent's key opens its own sector (§10).
pub struct Hub {
    sectors: Vec<(String, Arc<AppState>)>,
    /// The manifest and the file stamp it was read at; see `current_manifest`.
    manifest: Option<std::sync::Mutex<(sectors::Manifest, Option<db::Stamp>)>>,
    manifest_path: Option<PathBuf>,
    token: String,
    port: u16,
    /// Levels the single-database `/mcp` may read in full (`serve --clearance`).
    clearance: Vec<String>,
    /// What an agent without clearance is served from: the sector's records
    /// with the protected ones replaced (§11.4), rebuilt when its file changes.
    redacted: std::sync::Mutex<
        std::collections::HashMap<(String, String), (Option<db::Stamp>, Arc<Engine>)>,
    >,
}

/// A sector's engine as one caller may use it: the real one, or the redacted copy.
enum Held<'a> {
    Full(tokio::sync::RwLockReadGuard<'a, Engine>),
    Redacted(Arc<Engine>),
}

impl std::ops::Deref for Held<'_> {
    type Target = Engine;
    fn deref(&self) -> &Engine {
        match self {
            Held::Full(g) => g,
            Held::Redacted(a) => a,
        }
    }
}

async fn engine_for<'a>(
    h: &Hub,
    name: &str,
    state: &'a Arc<AppState>,
    clearance: &[String],
) -> Held<'a> {
    let full = current(state).await;
    if !sensitive::any_hidden(&full.records, clearance) {
        return Held::Full(full);
    }
    let key = (name.to_string(), clearance.join(","));
    let stamp = db::stamp(&state.path);
    if let Some((s, e)) = h.redacted.lock().unwrap().get(&key) {
        if *s == stamp {
            return Held::Redacted(e.clone());
        }
    }
    let engine = Arc::new(Engine::new(
        full.meta.clone(),
        sensitive::redact(&full.records, clearance),
    ));
    h.redacted
        .lock()
        .unwrap()
        .insert(key, (stamp, engine.clone()));
    Held::Redacted(engine)
}

impl Hub {
    /// The manifest as it is on disk now: a rotated key stops working, and a
    /// changed `reads` applies, on the very next request -- no restart. A
    /// manifest that no longer parses is refused and the last good one kept.
    fn current_manifest(&self) -> Option<sectors::Manifest> {
        let (cell, path) = (self.manifest.as_ref()?, self.manifest_path.as_ref()?);
        let mut held = cell.lock().unwrap();
        let now = db::stamp(path);
        if now != held.1 {
            match sectors::Manifest::load(path) {
                Ok(m) => *held = (m, now),
                Err(e) => eprintln!("fds: {e}; still using the manifest as it was"),
            }
        }
        Some(held.0.clone())
    }
}

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

fn fail(code: StatusCode, msg: impl Into<String>) -> ApiError {
    (code, Json(json!({ "error": msg.into() })))
}

/// 128 bits from two independently keyed SipHash instances. `RandomState`
/// seeds its keys from the operating system's RNG, so the output cannot be
/// predicted from outside the process -- which is all a CSRF token for a
/// loopback server needs, without pulling in a crate for it.
pub(crate) fn new_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (0..2u64)
        .map(|i| {
            let mut h = RandomState::new().build_hasher();
            h.write_u64(i);
            h.write_u128(nanos);
            h.write_u32(std::process::id());
            format!("{:016x}", h.finish())
        })
        .collect()
}

pub fn app_with(
    path: PathBuf,
    engine: Engine,
    port: u16,
    clearance: Vec<String>,
) -> (Router, String) {
    let token = new_token();
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "db".into());
    let state = sector_state(path, engine, token.clone(), port, String::new());
    let hub = Arc::new(Hub {
        sectors: vec![(name, state.clone())],
        manifest: None,
        manifest_path: None,
        token: token.clone(),
        port,
        clearance,
        redacted: Default::default(),
    });
    (sector_router(state).merge(hub_router(hub, None)), token)
}

fn sector_state(
    path: PathBuf,
    engine: Engine,
    token: String,
    port: u16,
    base: String,
) -> Arc<AppState> {
    let stamp = std::sync::Mutex::new(db::stamp(&path));
    Arc::new(AppState {
        engine: RwLock::new(engine),
        stamp,
        path,
        token,
        port,
        base,
    })
}

/// One database's API and manager. Mounted at `/`, or at `/s/<sector>`.
fn sector_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(get_ui))
        .route("/assets/vt323.woff2", get(get_font))
        .route("/status", get(get_status))
        .route("/query", post(post_query))
        .route("/check", post(post_check))
        .route("/reload", post(post_reload))
        .route("/records", get(get_records))
        .route("/records/:id", get(get_record))
        .route("/records/:id", delete(delete_record).patch(patch_record))
        .route("/facts", post(post_fact))
        .route("/procedures", post(post_procedures))
        .route("/embed", post(post_embed))
        .route(
            "/import",
            post(post_import).layer(DefaultBodyLimit::max(MAX_IMPORT_BYTES)),
        )
        .route("/export", get(get_export))
        .route("/contexts", get(get_contexts).post(post_context))
        .route("/contexts/:name", get(get_context))
        .route("/graph", get(get_graph))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

/// `/mcp` for every agent and, with sectors, the index page at `/`.
fn hub_router(hub: Arc<Hub>, index: Option<String>) -> Router {
    let mut r = Router::new().route(
        "/mcp",
        post(post_mcp).get(mcp_not_allowed).delete(mcp_not_allowed),
    );
    if let Some(page) = index {
        r = r.route(
            "/",
            get(move || async move {
                let mut res = page.into_response();
                res.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                );
                res
            }),
        );
    }
    r.layer(middleware::from_fn_with_state(hub.clone(), hub_guard))
        .with_state(hub)
}

pub async fn run(
    path: PathBuf,
    port: u16,
    open: bool,
    clearance: Vec<String>,
) -> Result<(), String> {
    let (meta, records) = db::load(&path)?;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let (router, token) = app_with(path.clone(), Engine::new(meta, records), port, clearance);

    let url = format!("http://127.0.0.1:{port}/");
    eprintln!("fds: serving {}", path.display());
    eprintln!("     manager   {url}");
    eprintln!("     API       {url}query, {url}status");
    eprintln!(
        "     MCP       {url}mcp  (reads open; memory tools with `Authorization: Bearer <token>`)"
    );
    eprintln!(
        "     writes need the header `{TOKEN_HEADER}: {token}` (the manager sends it for you)"
    );
    if open {
        open_browser(&url);
    }
    axum::serve(listener, router)
        .await
        .map_err(|e| e.to_string())
}

/// `fds serve --sectors`: every sector's file loaded once, each with its own
/// manager at `/s/<name>/`, and one `/mcp` where each agent's key opens its
/// own sector and the ones it reads (§10).
pub async fn run_sectors(manifest_path: PathBuf, port: u16, open: bool) -> Result<(), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let (router, hub) = sectors_app(&manifest_path, port)?;
    let url = format!("http://127.0.0.1:{port}");
    eprintln!(
        "fds: serving {} sector(s) from {}",
        hub.sectors.len(),
        manifest_path.display()
    );
    for (name, _) in &hub.sectors {
        eprintln!("     {name:<12} {url}/s/{name}/");
    }
    eprintln!("     MCP          {url}/mcp  (each agent sends its own key)");
    eprintln!(
        "     manager writes need the header `{TOKEN_HEADER}: {}`",
        hub.token
    );
    if open {
        open_browser(&format!("{url}/"));
    }
    axum::serve(listener, router)
        .await
        .map_err(|e| e.to_string())
}

fn sectors_app(manifest_path: &std::path::Path, port: u16) -> Result<(Router, Arc<Hub>), String> {
    let manifest = sectors::Manifest::load(manifest_path)?;
    if manifest.sectors.is_empty() {
        return Err(format!(
            "{} declares no sectors; add one with `fds sector new <name>`.",
            manifest_path.display()
        ));
    }
    let token = new_token();
    let url = format!("http://127.0.0.1:{port}");

    let mut router = Router::new();
    let mut loaded = Vec::new();
    let mut rows = String::new();
    for def in &manifest.sectors {
        let path = manifest.path_of(def);
        let (meta, records) = db::load(&path)?;
        let n = records.len();
        let base = format!("/s/{}", def.name);
        let state = sector_state(
            path,
            Engine::new(meta, records),
            token.clone(),
            port,
            base.clone(),
        );
        router = router.nest(&base, sector_router(state.clone()));
        loaded.push((def.name.clone(), state));
        // Names are [a-z0-9_-] (sectors::valid_name): safe in HTML as they are.
        let role = if def.key_sha256.is_some() {
            "agent"
        } else {
            "shared"
        };
        let reads = if def.reads.is_empty() {
            "—".to_string()
        } else {
            def.reads.join(", ")
        };
        rows.push_str(&format!(
            "<tr><td><a href=\"{base}\">{}</a></td><td>{role}</td><td>{reads}</td><td>{n}</td></tr>",
            def.name
        ));
    }
    let index = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>FDS sectors</title>\
<style>body{{font:15px system-ui,sans-serif;margin:2rem auto;max-width:44rem;padding:0 1rem;color:#10243C}}\
table{{border-collapse:collapse;width:100%}}td,th{{text-align:left;padding:.4rem .8rem .4rem 0;border-bottom:1px solid #d5e0ec}}\
code{{background:#eef3f8;padding:0 .3em;border-radius:4px}}</style>\
<h1>FDS sectors</h1><table><tr><th>Sector</th><th>Role</th><th>Reads</th><th>Records</th></tr>{rows}</table>\
<p>Agents connect to <code>{url}/mcp</code> with <code>Authorization: Bearer &lt;key&gt;</code>, \
or through <code>fds mcp --connect {url}/mcp</code> with the key in <code>FDS_KEY</code>.</p>"
    );
    let stamp = db::stamp(manifest_path);
    let hub = Arc::new(Hub {
        sectors: loaded,
        manifest: Some(std::sync::Mutex::new((manifest, stamp))),
        manifest_path: Some(manifest_path.to_path_buf()),
        token,
        port,
        clearance: Vec::new(),
        redacted: Default::default(),
    });
    Ok((router.merge(hub_router(hub.clone(), Some(index))), hub))
}

fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if r.is_err() {
        eprintln!("fds: could not open a browser; visit {url}");
    }
}

/// Host check on every request; token check on every write.
async fn guard(State(s): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if let Err(e) = host_ok(&req, s.port) {
        return fail(StatusCode::FORBIDDEN, e).into_response();
    }

    let writes = matches!(
        *req.method(),
        Method::POST | Method::DELETE | Method::PUT | Method::PATCH
    );
    // /query and /check only read, and /reload only re-reads the file; SPEC §8
    // lets any local script call them without the token.
    let read_only_post = matches!(req.uri().path(), "/query" | "/check" | "/reload");
    if writes && !read_only_post {
        let sent = req
            .headers()
            .get(TOKEN_HEADER)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        if !constant_time_eq(sent.as_bytes(), s.token.as_bytes()) {
            return fail(
                StatusCode::FORBIDDEN,
                format!("writes need a valid `{TOKEN_HEADER}` header"),
            )
            .into_response();
        }
    }
    next.run(req).await
}

fn host_ok(req: &Request, port: u16) -> Result<(), String> {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let allowed = [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ];
    if allowed.iter().any(|a| a == host) {
        Ok(())
    } else {
        Err(format!(
            "refusing Host `{host}`; this server only answers on loopback"
        ))
    }
}

/// The same loopback rule for `/mcp` and the index; the endpoint checks
/// `Origin` and the caller's key itself.
async fn hub_guard(State(h): State<Arc<Hub>>, req: Request, next: Next) -> Response {
    if let Err(e) = host_ok(&req, h.port) {
        return fail(StatusCode::FORBIDDEN, e).into_response();
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Read-modify-write, always from disk and always under the write lock.
/// Loading first means a `fds fact add` run from the command line while the
/// manager is open is kept, not overwritten by a stale in-memory copy.
async fn write<F>(s: &AppState, f: F) -> Result<Value, ApiError>
where
    F: FnOnce(&mut DbMeta, &mut Vec<Record>) -> Result<Value, String>,
{
    let mut engine = s.engine.write().await;
    let _lock = db::lock(&s.path).map_err(|e| fail(StatusCode::SERVICE_UNAVAILABLE, e))?;
    let (mut meta, mut records) =
        db::load(&s.path).map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let out = f(&mut meta, &mut records).map_err(|e| fail(StatusCode::BAD_REQUEST, e))?;
    db::save(&s.path, &mut meta, &records)
        .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    *engine = Engine::new(meta, records);
    *s.stamp.lock().unwrap() = db::stamp(&s.path);
    Ok(out)
}

/// The engine, re-read first if the file changed since it was built: a
/// `fds fact add`, another server or an agent may have written it. Costs one
/// `stat` per request when nothing changed.
async fn current(s: &AppState) -> tokio::sync::RwLockReadGuard<'_, Engine> {
    let now = db::stamp(&s.path);
    if now != *s.stamp.lock().unwrap() {
        let mut engine = s.engine.write().await;
        if now != *s.stamp.lock().unwrap() {
            if let Ok((meta, records)) = db::load(&s.path) {
                *engine = Engine::new(meta, records);
                *s.stamp.lock().unwrap() = now;
            }
        }
    }
    s.engine.read().await
}

// ── MCP over HTTP (§9.2) ──────────────────────────────────────────────────

fn rpc_failure(code: StatusCode, id: Option<Value>, rpc: i64, message: String) -> Response {
    let body = json!({"jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": {"code": rpc, "message": message}});
    (code, Json(body)).into_response()
}

/// One JSON-RPC message per POST, answered with one JSON object. Stateless:
/// no session is minted or expected, as in protocol revision 2026-07-28;
/// a legacy client's `initialize` is answered all the same.
async fn post_mcp(
    State(h): State<Arc<Hub>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // A web page can reach a loopback server; it cannot fake its Origin.
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let ours = [
            format!("http://127.0.0.1:{}", h.port),
            format!("http://localhost:{}", h.port),
            format!("http://[::1]:{}", h.port),
        ];
        if !ours.iter().any(|o| o == origin) {
            return rpc_failure(
                StatusCode::FORBIDDEN,
                None,
                -32600,
                format!("refusing Origin `{origin}`"),
            );
        }
    }
    let req: Value = match serde_json::from_slice::<Value>(&body) {
        Ok(v) if v.is_object() => v,
        Ok(_) => {
            return rpc_failure(
                StatusCode::BAD_REQUEST,
                None,
                -32600,
                "one JSON-RPC message per request; batches are not supported".into(),
            )
        }
        Err(e) => {
            return rpc_failure(
                StatusCode::BAD_REQUEST,
                None,
                -32700,
                format!("parse error: {e}"),
            )
        }
    };
    let id = req.get("id").cloned();
    let read_header = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    if let Err(e) = mcp::check_headers(&req, read_header) {
        return (StatusCode::BAD_REQUEST, Json(mcp::header_mismatch(id, &e))).into_response();
    }

    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    let (names, memory, clear): (Vec<String>, Option<mcp::Memory>, Vec<String>) = match h
        .current_manifest()
    {
        Some(m) => {
            let Some(agent) = bearer.and_then(|k| m.agent_for_key(k)) else {
                let mut res = rpc_failure(
                    StatusCode::UNAUTHORIZED,
                    id,
                    -32001,
                    "an agent key is required: Authorization: Bearer <key> (fds sector new <name> --agent makes one)".into(),
                );
                res.headers_mut()
                    .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                return res;
            };
            if !h.sectors.iter().any(|(n, _)| n == &agent.name) {
                return rpc_failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    id,
                    -32002,
                    format!("sector `{}` was added after this server started; restart `fds serve --sectors`", agent.name),
                );
            }
            let memory = mcp::Memory {
                path: m.path_of(agent),
                sector: agent.name.clone(),
                writer: format!("model:{}", agent.name),
            };
            (m.view_of(agent), Some(memory), agent.clearance.clone())
        }
        None => {
            let (name, state) = &h.sectors[0];
            let writes = bearer.map_or(false, |b| {
                constant_time_eq(b.as_bytes(), h.token.as_bytes())
            });
            let memory = writes.then(|| mcp::Memory {
                path: state.path.clone(),
                sector: name.clone(),
                writer: "model:agent".into(),
            });
            (vec![name.clone()], memory, h.clearance.clone())
        }
    };
    if id.is_none() {
        return StatusCode::ACCEPTED.into_response();
    }

    let mut held = Vec::with_capacity(names.len());
    for n in &names {
        if let Some((name, state)) = h.sectors.iter().find(|(name, _)| name == n) {
            held.push((name.as_str(), engine_for(&h, name, state, &clear).await));
        }
    }
    let session = mcp::Session {
        views: held
            .iter()
            .map(|(name, engine)| View {
                name,
                engine: &**engine,
            })
            .collect(),
        memory: memory.as_ref(),
    };
    let modern = mcp::requested_version(&req).is_some();
    let Some(resp) = mcp::respond(&req, &session).await else {
        return StatusCode::ACCEPTED.into_response();
    };
    let code = match resp.pointer("/error/code").and_then(|c| c.as_i64()) {
        Some(-32601) if modern => StatusCode::NOT_FOUND,
        Some(-32022) => StatusCode::BAD_REQUEST,
        _ => StatusCode::OK,
    };
    (code, Json(resp)).into_response()
}

/// The 2026-07-28 transport has no GET stream and no session to DELETE.
async fn mcp_not_allowed() -> Response {
    let mut res = StatusCode::METHOD_NOT_ALLOWED.into_response();
    res.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    res
}

// ── pages ─────────────────────────────────────────────────────────────────

async fn get_ui(State(s): State<Arc<AppState>>) -> Response {
    let doc_ext = serde_json::to_string(&crate::ingest::document_extensions())
        .unwrap_or_else(|_| "[]".into());
    let img_ext =
        serde_json::to_string(&crate::media::image_extensions()).unwrap_or_else(|_| "[]".into());
    let mut res = UI
        .replace("__FDS_TOKEN__", &s.token)
        .replace("__FDS_DOC_EXT__", &doc_ext)
        .replace("__FDS_IMG_EXT__", &img_ext)
        .replace("__FDS_VISION_MODEL__", &crate::media::vision_model())
        .replace("__FDS_TABULAR__", &crate::tabular::client_json())
        .replace("__FDS_BASE__", &s.base)
        .replace(
            "url('/assets/vt323.woff2')",
            &format!("url('{}/assets/vt323.woff2')", s.base),
        )
        .into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // The page carries the write token: never cache it, never frame it.
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline';              script-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none';              form-action 'none'; frame-ancestors 'none'",
        ),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    res
}

async fn get_font() -> Response {
    let mut res = Body::from(FONT_VT323).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("font/woff2"));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    res
}

// ── reads ─────────────────────────────────────────────────────────────────

async fn get_status(State(s): State<Arc<AppState>>) -> Json<Value> {
    let engine = current(&s).await;
    Json(status::summary(&s.path, &engine.meta, &engine.records))
}

#[derive(Deserialize)]
struct QueryBody {
    q: String,
    #[serde(default)]
    collection: Option<String>,
    #[serde(default)]
    budget: Option<u32>,
    /// Also return what the results link to, one hop out (SPEC §4.4).
    #[serde(default)]
    links: bool,
    /// Only records verified by these: `human`, `model:`, ... comma-separated.
    #[serde(default)]
    verified_by: Option<String>,
}

async fn post_query(State(s): State<Arc<AppState>>, Json(body): Json<QueryBody>) -> ApiResult {
    if body.q.trim().is_empty() {
        return Err(fail(StatusCode::BAD_REQUEST, "empty query"));
    }
    let engine = current(&s).await;
    let emb = if engine.has_embeddings() {
        ollama::embed_query(&engine.meta.embedding_model, &body.q).await
    } else {
        None
    };
    let started = std::time::Instant::now();
    let verified = verified_list(body.verified_by.as_deref());
    let scope = Scope {
        collection: body.collection.as_deref(),
        verified_by: &verified,
    };
    let mut result = engine.query_scoped(
        &body.q,
        &scope,
        body.budget.unwrap_or(DEFAULT_BUDGET),
        emb.as_deref(),
    );
    if body.links {
        engine.expand_links(&mut result, &scope);
    }
    let mut v = serde_json::to_value(&result).unwrap_or_default();
    v["engine_ms"] = json!((started.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0);
    v["used_vectors"] = json!(emb.is_some());
    Ok(Json(v))
}

#[derive(Deserialize)]
struct CheckBody {
    subject: String,
    action: String,
    #[serde(default)]
    value: Option<Value>,
}

/// SPEC §11.2: allow or deny, decided by comparing and not by a model, from
/// the policies of this database that a person verified. Advisory: the guard
/// is what enforces, and it applies its manifest's default where this
/// answers `default_applied`.
async fn post_check(State(s): State<Arc<AppState>>, Json(b): Json<CheckBody>) -> ApiResult {
    if b.subject.trim().is_empty() || b.action.trim().is_empty() {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "subject and action are required",
        ));
    }
    let value = match &b.value {
        Some(Value::String(v)) => Some(v.trim().to_string()).filter(|v| !v.is_empty()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(v)) => Some(v.to_string()),
        _ => None,
    };
    let engine = current(&s).await;
    let d = policy::check(
        &policy::policies(&engine.records),
        b.subject.trim(),
        b.action.trim(),
        value.as_deref(),
        policy::Fallback::Allow,
    );
    Ok(Json(serde_json::to_value(&d).unwrap_or_default()))
}

#[derive(Deserialize)]
struct ListParams {
    kind: Option<String>,
    #[serde(rename = "type")]
    rtype: Option<String>,
    /// Who verified them, as in a query (SPEC §4): `human`, `model:`, ...
    verified_by: Option<String>,
    collection: Option<String>,
    search: Option<String>,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

async fn get_records(State(s): State<Arc<AppState>>, Query(p): Query<ListParams>) -> ApiResult {
    let kind = match p.kind.as_deref().filter(|k| !k.is_empty()) {
        Some(k) => Some(
            Kind::parse(k)
                .ok_or_else(|| fail(StatusCode::BAD_REQUEST, format!("unknown kind `{k}`")))?,
        ),
        None => None,
    };
    let needle = p
        .search
        .as_deref()
        .map(crate::text::search_fold)
        .filter(|n| !n.is_empty());
    let engine = current(&s).await;
    let matching: Vec<&Record> = engine
        .records
        .iter()
        .filter(|r| kind.map_or(true, |k| r.kind == k))
        .filter(|r| {
            p.rtype
                .as_deref()
                .filter(|t| !t.is_empty())
                .map_or(true, |t| r.rtype == t)
        })
        .filter(|r| {
            let who = verified_list(p.verified_by.as_deref());
            who.is_empty()
                || who.iter().any(|v| {
                    if v.ends_with(':') {
                        r.verified_by.starts_with(v.as_str())
                    } else {
                        r.verified_by == *v
                    }
                })
        })
        .filter(|r| {
            p.collection
                .as_deref()
                .filter(|c| !c.is_empty())
                .map_or(true, |c| r.collection == c)
        })
        .filter(|r| {
            needle.as_deref().map_or(true, |n| {
                r.id.starts_with(n)
                    || r.question
                        .as_deref()
                        .map_or(false, |q| crate::text::search_fold(q).contains(n))
                    || crate::text::search_fold(&r.title).contains(n)
                    || crate::text::search_fold(&r.body).contains(n)
                    || crate::text::search_fold(&r.source).contains(n)
            })
        })
        .collect();
    let limit = p.limit.unwrap_or(200).min(MAX_LIST);
    let rows: Vec<Value> = matching
        .iter()
        .skip(p.offset)
        .take(limit)
        .map(|r| {
            let mut v = ops::record_json(r);
            // Listings carry a preview; the full body is one click away.
            let preview: String = r.body.chars().take(160).collect();
            v["body"] = json!(preview);
            v["truncated"] = json!(r.body.chars().count() > 160);
            v
        })
        .collect();
    Ok(Json(
        json!({ "total": matching.len(), "offset": p.offset, "limit": limit, "rows": rows }),
    ))
}

async fn get_record(State(s): State<Arc<AppState>>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let engine = current(&s).await;
    let i = db::resolve_id(&engine.records, &id).map_err(|e| fail(StatusCode::NOT_FOUND, e))?;
    let mut v = ops::record_json(&engine.records[i]);
    // `links` on disk are only the explicit ones; here every link, however
    // written, resolved -- and who points back.
    v["links_explicit"] = v["links"].take();
    v["links"] = json!(engine.links_of(i));
    v["backlinks"] = json!(engine
        .backlinks_of(i)
        .iter()
        .map(|&j| json!({"id": engine.records[j].id, "title": engine.records[j].title, "type": engine.records[j].rtype}))
        .collect::<Vec<_>>());
    Ok(Json(v))
}

// ── writes ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct FactBody {
    question: String,
    answer: String,
    #[serde(default)]
    collection: Option<String>,
    #[serde(default)]
    source: Option<String>,
}

async fn post_fact(State(s): State<Arc<AppState>>, Json(b): Json<FactBody>) -> ApiResult {
    if b.question.trim().is_empty() || b.answer.trim().is_empty() {
        return Err(fail(
            StatusCode::BAD_REQUEST,
            "a fact needs both a question and an answer",
        ));
    }
    let collection = b
        .collection
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "default".into());
    let source = b
        .source
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "manager".into());
    let out = write(&s, |_, records| {
        let id = ops::upsert_fact(
            records,
            &b.question,
            &b.answer,
            &collection,
            &source,
            "human",
            &today(),
        )?;
        Ok(json!({ "id": id }))
    })
    .await?;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct ProcParams {
    collection: Option<String>,
}

async fn post_procedures(
    State(s): State<Arc<AppState>>,
    Query(p): Query<ProcParams>,
    Json(body): Json<Value>,
) -> ApiResult {
    let collection = p
        .collection
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "default".into());
    let out = write(&s, |_, records| {
        let ids = ops::add_procedures_value(body, records, &collection, "manager")?;
        Ok(json!({ "ids": ids }))
    })
    .await?;
    Ok(Json(out))
}

/// An edit by a person (SPEC §8.2): content, title, type, attrs or links --
/// or none of them, to mark the record verified as it stands.
async fn patch_record(
    State(s): State<Arc<AppState>>,
    UrlPath(id): UrlPath<String>,
    Json(e): Json<ops::Edit>,
) -> ApiResult {
    let out = write(&s, |_, records| {
        Ok(json!({ "id": ops::edit(records, &id, e, &today())? }))
    })
    .await?;
    Ok(Json(out))
}

async fn delete_record(State(s): State<Arc<AppState>>, UrlPath(id): UrlPath<String>) -> ApiResult {
    let out = write(&s, |_, records| {
        let removed = ops::remove(records, &id)?;
        Ok(json!({ "removed": removed }))
    })
    .await?;
    Ok(Json(out))
}

async fn get_contexts(State(s): State<Arc<AppState>>) -> Json<Value> {
    let engine = current(&s).await;
    let list: Vec<Value> = engine
        .contexts()
        .into_iter()
        .map(|i| {
            let b = engine.context(i);
            json!({
                "id": b.id, "name": b.name, "collection": b.collection,
                "records": b.records.len(), "missing": b.missing.len(),
                "tokens": b.tokens, "hash": b.hash,
            })
        })
        .collect();
    Json(json!(list))
}

#[derive(Deserialize)]
struct ContextParams {
    /// The hash the caller already holds (SPEC §4.4).
    known: Option<String>,
}

/// One context, loaded whole -- unless the caller already holds this exact
/// version, in which case it gets a few bytes saying so instead of the lot.
/// That is the point of a context: an agent reads it once, not every turn.
async fn get_context(
    State(s): State<Arc<AppState>>,
    UrlPath(name): UrlPath<String>,
    Query(p): Query<ContextParams>,
    headers: axum::http::HeaderMap,
) -> Response {
    let engine = current(&s).await;
    let Some(i) = engine.find_context(&name) else {
        return fail(StatusCode::NOT_FOUND, format!("no context named `{name}`")).into_response();
    };
    let b = engine.context(i);
    let etag = format!("\"{}\"", b.hash);
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok());
    if if_none_match == Some(etag.as_str()) {
        let mut r = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(v) = HeaderValue::from_str(&etag) {
            r.headers_mut().insert(header::ETAG, v);
        }
        return r;
    }
    let body = if p.known.as_deref() == Some(b.hash.as_str()) {
        json!({ "name": b.name, "hash": b.hash, "tokens": b.tokens, "changed": false })
    } else {
        let mut v = serde_json::to_value(&b).unwrap_or_default();
        v["changed"] = json!(true);
        v
    };
    let mut r = Json(body).into_response();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        r.headers_mut().insert(header::ETAG, v);
    }
    r
}

#[derive(Deserialize)]
struct ContextBody {
    name: String,
    #[serde(default)]
    instructions: String,
    #[serde(default)]
    links: Vec<String>,
    #[serde(default)]
    collection: Option<String>,
}

async fn post_context(State(s): State<Arc<AppState>>, Json(b): Json<ContextBody>) -> ApiResult {
    let collection = b
        .collection
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "default".into());
    let out = write(&s, |_, records| {
        let r = ops::context_record(
            &b.name,
            &b.instructions,
            &b.links,
            &collection,
            "manager",
            "human",
            &today(),
        )?;
        Ok(json!({ "id": ops::upsert(records, r) }))
    })
    .await?;
    Ok(Json(out))
}

/// Every record as a node and every resolved link as an edge: the graph.
async fn get_graph(State(s): State<Arc<AppState>>) -> Json<Value> {
    let engine = current(&s).await;
    let nodes: Vec<Value> = engine
        .records
        .iter()
        .map(|r| json!({"id": r.id, "title": r.title, "kind": r.kind, "type": r.rtype, "collection": r.collection}))
        .collect();
    let edges: Vec<Value> = engine
        .edges()
        .into_iter()
        .map(|(a, b)| json!([a, b]))
        .collect();
    Json(json!({ "nodes": nodes, "edges": edges }))
}

/// Plans an import and, unless it is a dry run, applies it (SPEC §8.3).
/// Refused with 422 -- and nothing written -- when rows are invalid and the
/// caller asked for all-or-nothing.
async fn post_import(
    State(s): State<Arc<AppState>>,
    Json(req): Json<etl::ImportRequest>,
) -> Response {
    let mut engine = s.engine.write().await;
    let _lock = match db::lock(&s.path) {
        Ok(l) => l,
        Err(e) => return fail(StatusCode::SERVICE_UNAVAILABLE, e).into_response(),
    };
    let (mut meta, mut records) = match db::load(&s.path) {
        Ok(x) => x,
        Err(e) => return fail(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    // Images are described here, on a real run only, before anything is
    // written. The write lock is held meanwhile: HTTP queries wait for the
    // model (MCP reads the file in its own process and does not).
    let model = req
        .vision_model
        .clone()
        .unwrap_or_else(crate::media::vision_model);
    let plan = etl::run_async(req, &mut records, |bytes, prompt| {
        let model = model.clone();
        async move { ollama::describe_image(&model, &bytes, &prompt).await }
    })
    .await;
    if plan.written {
        if let Err(e) = db::save(&s.path, &mut meta, &records) {
            return fail(StatusCode::INTERNAL_SERVER_ERROR, e).into_response();
        }
        *engine = Engine::new(meta, records);
        *s.stamp.lock().unwrap() = db::stamp(&s.path);
    }
    let code = if plan.blocked() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    (code, Json(serde_json::to_value(&plan).unwrap_or_default())).into_response()
}

#[derive(Deserialize)]
struct ExportParams {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    delimiter: Option<String>,
    kind: Option<String>,
    collection: Option<String>,
}

/// Every matching record, in the shape `/import` reads back.
async fn get_export(State(s): State<Arc<AppState>>, Query(p): Query<ExportParams>) -> Response {
    let kind = match p.kind.as_deref().filter(|k| !k.is_empty()) {
        Some(k) => match Kind::parse(k) {
            Some(k) => Some(k),
            None => {
                return fail(StatusCode::BAD_REQUEST, format!("unknown kind `{k}`")).into_response()
            }
        },
        None => None,
    };
    let engine = current(&s).await;
    let rows: Vec<&Record> = engine
        .records
        .iter()
        .filter(|r| kind.map_or(true, |k| r.kind == k))
        .filter(|r| {
            p.collection
                .as_deref()
                .filter(|c| !c.is_empty())
                .map_or(true, |c| r.collection == c)
        })
        .collect();
    let stem: String = engine
        .meta
        .name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let stem = if stem.is_empty() {
        "fds".to_string()
    } else {
        stem
    };

    let (body, mime, ext) = match p.format.as_deref().unwrap_or("json") {
        "csv" => {
            let delim = if p.delimiter.as_deref() == Some("semicolon") {
                ';'
            } else {
                ','
            };
            (
                etl::export_csv(&rows, delim),
                "text/csv; charset=utf-8",
                "csv",
            )
        }
        "json" => {
            let v: Vec<Value> = rows.iter().map(|r| etl::export_row(r)).collect();
            (
                serde_json::to_string_pretty(&v).unwrap_or_default(),
                "application/json",
                "json",
            )
        }
        other => {
            return fail(
                StatusCode::BAD_REQUEST,
                format!("unknown format `{other}`: json or csv"),
            )
            .into_response()
        }
    };
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{stem}.{ext}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

#[derive(Deserialize, Default)]
struct EmbedBody {
    #[serde(default)]
    all: bool,
    #[serde(default)]
    model: Option<String>,
}

/// Holds the write lock across the Ollama call: queries wait for the few
/// seconds it takes, rather than racing a half-embedded database.
async fn post_embed(State(s): State<Arc<AppState>>, body: Option<Json<EmbedBody>>) -> ApiResult {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let mut engine = s.engine.write().await;
    let _lock = db::lock(&s.path).map_err(|e| fail(StatusCode::SERVICE_UNAVAILABLE, e))?;
    let (mut meta, mut records) =
        db::load(&s.path).map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let n = ops::embed(&mut meta, &mut records, b.model, b.all)
        .await
        .map_err(|e| fail(StatusCode::BAD_GATEWAY, e))?;
    if n > 0 {
        db::save(&s.path, &mut meta, &records)
            .map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    let summary = status::summary(&s.path, &meta, &records);
    *engine = Engine::new(meta, records);
    *s.stamp.lock().unwrap() = db::stamp(&s.path);
    Ok(Json(json!({ "embedded": n, "status": summary })))
}

/// Re-reads the file after an external write, without a restart.
async fn post_reload(State(s): State<Arc<AppState>>) -> ApiResult {
    let (meta, records) =
        db::load(&s.path).map_err(|e| fail(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let summary = status::summary(&s.path, &meta, &records);
    *s.engine.write().await = Engine::new(meta, records);
    *s.stamp.lock().unwrap() = db::stamp(&s.path);
    Ok(Json(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real server on an ephemeral loopback port, over a scratch database.
    async fn spawn() -> (String, String, PathBuf) {
        let path = std::env::temp_dir().join(format!("fds-serve-test-{}.fds", new_token()));
        let mut meta = DbMeta {
            name: "t".into(),
            ..Default::default()
        };
        db::save(&path, &mut meta, &[]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (meta, records) = db::load(&path).unwrap();
        let (router, token) = app_with(path.clone(), Engine::new(meta, records), port, Vec::new());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (format!("http://127.0.0.1:{port}"), token, path)
    }

    #[tokio::test]
    async fn writes_without_the_token_are_refused_and_change_nothing() {
        let (base, _, path) = spawn().await;
        let c = reqwest::Client::new();
        let r = c
            .post(format!("{base}/facts"))
            .json(&json!({"question": "q", "answer": "a"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        assert!(db::load(&path).unwrap().1.is_empty());
        let _ = std::fs::remove_file(path);
    }

    /// A write from another process (the CLI, an agent) is visible on the
    /// next request, with no /reload.
    #[tokio::test]
    async fn a_write_from_outside_is_seen_without_reload() {
        let (base, _, path) = spawn().await;
        {
            let _lock = db::lock(&path).unwrap();
            let (mut meta, mut records) = db::load(&path).unwrap();
            ops::upsert_fact(
                &mut records,
                "written outside",
                "by the CLI",
                "default",
                "cli",
                "human",
                "2026-10-12",
            )
            .unwrap();
            db::save(&path, &mut meta, &records).unwrap();
        }
        let q: Value = reqwest::Client::new()
            .post(format!("{base}/query"))
            .json(&json!({"q": "written outside"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(q["confidence"], "exact");
        let _ = std::fs::remove_file(path);
    }

    async fn rpc(c: &reqwest::Client, url: &str, key: Option<&str>, body: Value) -> (u16, Value) {
        let mut rb = c.post(url).json(&body);
        if let Some(k) = key {
            rb = rb.bearer_auth(k);
        }
        let res = rb.send().await.unwrap();
        let code = res.status().as_u16();
        (code, res.json().await.unwrap_or(Value::Null))
    }

    fn call(id: u64, tool: &str, args: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}})
    }

    fn text_of(v: &Value) -> String {
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// One database: reads are open, memory needs the manager's token.
    #[tokio::test]
    async fn mcp_over_http_on_one_database() {
        let (base, token, path) = spawn().await;
        let c = reqwest::Client::new();
        let url = format!("{base}/mcp");

        let (code, init) = rpc(&c, &url, None, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}})).await;
        assert_eq!(code, 200);
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        let (code, _) = rpc(
            &c,
            &url,
            None,
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        assert_eq!(code, 202);

        let (_, list) = rpc(
            &c,
            &url,
            None,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .await;
        assert_eq!(
            list["result"]["tools"].as_array().unwrap().len(),
            6,
            "no token: read-only"
        );
        let (_, list) = rpc(
            &c,
            &url,
            Some(&token),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
        )
        .await;
        assert_eq!(
            list["result"]["tools"].as_array().unwrap().len(),
            9,
            "token: memory tools too"
        );

        let (_, saved) = rpc(
            &c,
            &url,
            Some(&token),
            call(
                4,
                "fds_remember",
                json!({"question": "build server ip", "answer": "10.0.0.7"}),
            ),
        )
        .await;
        assert!(text_of(&saved).starts_with("saved"), "{saved}");
        let (_, found) = rpc(
            &c,
            &url,
            None,
            call(5, "fds_query", json!({"query": "build server ip"})),
        )
        .await;
        assert!(text_of(&found).contains("10.0.0.7") && text_of(&found).contains("model:agent"));

        let foreign = c
            .post(&url)
            .header("origin", "https://evil.example")
            .json(&json!({"jsonrpc": "2.0", "id": 6, "method": "ping"}))
            .send()
            .await
            .unwrap();
        assert_eq!(foreign.status(), 403);
        assert_eq!(c.get(&url).send().await.unwrap().status(), 405);

        let meta = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28"});
        let lying = c
            .post(&url)
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", "tools/list")
            .json(&json!({"jsonrpc": "2.0", "id": 7, "method": "ping", "params": {"_meta": meta}}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            lying.status(),
            400,
            "headers that disagree with the body are refused"
        );
        let _ = std::fs::remove_file(path);
    }

    /// Two agents and a shared sector: each sees its own memory and the
    /// shared one, never the other's; a key is required.
    #[tokio::test]
    async fn each_agent_sees_its_own_sector_and_the_shared_one() {
        let dir = std::env::temp_dir().join(format!(
            "fds-sectors-{}-{}",
            std::process::id(),
            new_token()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut shared = Vec::new();
        ops::upsert_fact(
            &mut shared,
            "refund window",
            "30 days from delivery",
            "default",
            "manual",
            "human",
            "2026-10-12",
        )
        .unwrap();
        db::save(&dir.join("shared.fds"), &mut DbMeta::default(), &shared).unwrap();
        db::save(&dir.join("sales.fds"), &mut DbMeta::default(), &[]).unwrap();
        db::save(&dir.join("ops.fds"), &mut DbMeta::default(), &[]).unwrap();
        let (k_sales, k_ops) = (sectors::new_key(), sectors::new_key());
        let manifest = dir.join("sectors.json");
        let mut m = sectors::Manifest::new(&manifest);
        m.sectors.push(sectors::SectorDef {
            name: "shared".into(),
            file: "shared.fds".into(),
            reads: vec![],
            key_sha256: None,
            clearance: vec![],
        });
        m.sectors.push(sectors::SectorDef {
            name: "sales".into(),
            file: "sales.fds".into(),
            reads: vec!["shared".into()],
            key_sha256: Some(sectors::key_hash(&k_sales)),
            clearance: vec![],
        });
        m.sectors.push(sectors::SectorDef {
            name: "ops".into(),
            file: "ops.fds".into(),
            reads: vec!["shared".into()],
            key_sha256: Some(sectors::key_hash(&k_ops)),
            clearance: vec![],
        });
        m.save(&manifest).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (router, _) = sectors_app(&manifest, port).unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let url = format!("http://127.0.0.1:{port}/mcp");
        let c = reqwest::Client::new();

        let (code, _) = rpc(
            &c,
            &url,
            None,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;
        assert_eq!(code, 401, "no key, no access");
        let (code, _) = rpc(
            &c,
            &url,
            Some("fdsk_wrong"),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .await;
        assert_eq!(code, 401);

        let (_, r) = rpc(
            &c,
            &url,
            Some(&k_sales),
            call(
                3,
                "fds_remember",
                json!({"question": "acme discount", "answer": "12 percent"}),
            ),
        )
        .await;
        assert!(text_of(&r).contains("sector sales"), "{r}");

        let (_, own) = rpc(
            &c,
            &url,
            Some(&k_sales),
            call(4, "fds_query", json!({"query": "acme discount"})),
        )
        .await;
        assert!(text_of(&own).contains("12 percent"));
        let (_, other) = rpc(
            &c,
            &url,
            Some(&k_ops),
            call(5, "fds_query", json!({"query": "acme discount"})),
        )
        .await;
        assert!(
            !text_of(&other).contains("12 percent"),
            "ops must not see sales' memory"
        );
        let (_, both) = rpc(
            &c,
            &url,
            Some(&k_ops),
            call(6, "fds_query", json!({"query": "refund window"})),
        )
        .await;
        assert!(text_of(&both).contains("30 days"), "everyone reads shared");

        assert_eq!(
            db::load(&dir.join("shared.fds")).unwrap().1.len(),
            1,
            "no agent wrote into shared"
        );
        assert_eq!(db::load(&dir.join("ops.fds")).unwrap().1.len(), 0);
        let index = c
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(index.contains("sales") && index.contains("/s/shared"));
        let status = c
            .get(format!("http://127.0.0.1:{port}/s/sales/status"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            status.status(),
            200,
            "each sector keeps its own manager and API"
        );

        // Rotating a key works on the running server: the old key is refused
        // at once, the new one opens the same sector.
        let rotated = sectors::new_key();
        let mut m = sectors::Manifest::load(&manifest).unwrap();
        m.get_mut("sales").unwrap().key_sha256 = Some(sectors::key_hash(&rotated));
        m.save(&manifest).unwrap();
        let (code, _) = rpc(
            &c,
            &url,
            Some(&k_sales),
            json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}),
        )
        .await;
        assert_eq!(code, 401, "the old key is revoked without a restart");
        let (_, own) = rpc(
            &c,
            &url,
            Some(&rotated),
            call(8, "fds_query", json!({"query": "acme discount"})),
        )
        .await;
        assert!(
            text_of(&own).contains("12 percent"),
            "the new key opens the same memory"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_foreign_host_header_is_refused_even_for_reads() {
        let (base, _, path) = spawn().await;
        let r = reqwest::Client::new()
            .get(format!("{base}/status"))
            .header("host", "evil.example:80")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn add_query_and_delete_a_fact_through_the_api() {
        let (base, token, path) = spawn().await;
        let c = reqwest::Client::new();

        let added: Value = c
            .post(format!("{base}/facts"))
            .header(TOKEN_HEADER, &token)
            .json(&json!({"question": "What is pin 4 of the NE555?", "answer": "RESET", "collection": "electronics"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = added["id"].as_str().expect("an id").to_string();

        // The engine was rebuilt: the new fact answers at stage 1.
        let q: Value = c
            .post(format!("{base}/query"))
            .json(&json!({"q": "what is pin 4 of the ne555"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(q["confidence"], "exact");
        assert_eq!(q["records"][0]["body"], "RESET");

        // And it is on disk, not only in memory.
        assert_eq!(db::load(&path).unwrap().1.len(), 1);

        let del = c
            .delete(format!("{base}/records/{}", &id[..8]))
            .header(TOKEN_HEADER, &token)
            .send()
            .await
            .unwrap();
        assert_eq!(del.status(), 200);
        assert!(db::load(&path).unwrap().1.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn import_previews_refuses_bad_batches_and_then_writes() {
        let (base, token, path) = spawn().await;
        let c = reqwest::Client::new();
        let import = |body: Value| {
            c.post(format!("{base}/import"))
                .header(TOKEN_HEADER, &token)
                .json(&body)
                .send()
        };
        let rows = json!([
            {"kind": "fact", "question": "lm317 vref", "answer": "1.25 V"},
            {"kind": "procedure", "intent": "flush the dns cache", "command": "ipconfig /flushdns"},
        ]);

        let preview: Value = import(json!({"dry_run": true, "rows": rows}))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(preview["summary"]["new"], 2);
        assert!(
            db::load(&path).unwrap().1.is_empty(),
            "a preview writes nothing"
        );

        let mut bad = rows.clone();
        bad.as_array_mut()
            .unwrap()
            .push(json!({"kind": "fact", "question": "no answer"}));
        let r = import(json!({"rows": bad})).await.unwrap();
        assert_eq!(r.status(), 422);
        assert!(
            db::load(&path).unwrap().1.is_empty(),
            "all-or-nothing means nothing"
        );

        let done: Value = import(json!({"rows": rows}))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(done["written"], true);
        assert_eq!(db::load(&path).unwrap().1.len(), 2);

        // Export, then import the export: nothing to do.
        let exported: Value = reqwest::get(format!("{base}/export?format=json"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let again: Value = import(json!({"rows": exported}))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(again["summary"]["unchanged"], 2, "{again}");
        assert_eq!(again["written"], false);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_context_is_read_once_then_answered_with_its_hash() {
        let (base, token, path) = spawn().await;
        let c = reqwest::Client::new();
        c.post(format!("{base}/facts"))
            .header(TOKEN_HEADER, &token)
            .json(&json!({"question": "router admin ip", "answer": "192.168.0.1"}))
            .send()
            .await
            .unwrap();
        let made = c.post(format!("{base}/contexts")).header(TOKEN_HEADER, &token)
            .json(&json!({"name": "Network support", "instructions": "Be brief. [[router admin ip]]", "links": ["printer drivers"]}))
            .send().await.unwrap();
        assert_eq!(made.status(), 200);

        let first = c
            .get(format!("{base}/contexts/network%20support"))
            .send()
            .await
            .unwrap();
        let etag = first
            .headers()
            .get("etag")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let b: Value = first.json().await.unwrap();
        assert_eq!(b["records"][0]["body"], "192.168.0.1");
        assert_eq!(b["missing"], json!(["printer drivers"]));
        let hash = b["hash"].as_str().unwrap().to_string();

        // Already held: a few bytes, not the bundle -- both ways of asking.
        let again: Value = c
            .get(format!("{base}/contexts/Network%20support?known={hash}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(again["changed"], false);
        assert!(again.get("records").is_none());
        let cached = c
            .get(format!("{base}/contexts/Network%20support"))
            .header("if-none-match", &etag)
            .send()
            .await
            .unwrap();
        assert_eq!(cached.status(), 304);

        // A context is never an answer to a query.
        let q: Value = c
            .post(format!("{base}/query"))
            .json(&json!({"q": "be brief network support"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(q["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["type"] != "context"));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn the_manager_page_carries_the_token_and_refuses_framing() {
        let (base, token, path) = spawn().await;
        let r = reqwest::get(format!("{base}/")).await.unwrap();
        let csp = r
            .headers()
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let html = r.text().await.unwrap();
        assert!(html.contains(&token));
        assert!(!html.contains("__FDS_TOKEN__"));
        assert!(
            !html.contains("__FDS_DOC_EXT__") && html.contains(r#""markdown""#),
            "the extension list is injected"
        );
        assert!(csp.contains("frame-ancestors 'none'"));
        let _ = std::fs::remove_file(path);
    }
}
