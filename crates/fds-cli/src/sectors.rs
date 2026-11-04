//! Sectors (SPEC.md §10): one database file per agent.
//!
//! Each agent keeps its own memory in its own file, reads shared knowledge it
//! cannot change, and is answered only from what it may read. A write from one
//! agent can never land in, or erase, another's file. One `fds serve --sectors`
//! process loads every file once, however many agents connect.
//!
//! The layout lives in a manifest, `sectors.json`, next to the files:
//!
//! ```json
//! { "sectors": [
//!     { "name": "shared", "file": "shared.fds" },
//!     { "name": "sales",  "file": "sales.fds", "reads": ["shared"], "key_sha256": "…" } ] }
//! ```
//!
//! A sector with a key is an agent's home. The key itself is shown once, when
//! it is made; the manifest keeps only its SHA-256.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ollama;
use crate::query::{Confidence, Engine, QueryResult, Scope};

pub const MANIFEST: &str = "sectors.json";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SectorDef {
    pub name: String,
    /// Relative to the manifest's folder.
    pub file: String,
    /// Other sectors this one may read, in the order they are searched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    /// SHA-256 of the agent's key, in hex. Present when an agent lives here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_sha256: Option<String>,
    /// Sensitivity levels this sector may read in full (§11.4); a record of any
    /// other level reaches its agent only as a stand-in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clearance: Vec<String>,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct Manifest {
    pub sectors: Vec<SectorDef>,
    /// What the guard and `fds check` answer when no policy matches: `allow`
    /// (the default) or `deny`. Subjects that must never fail open say `deny`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip)]
    dir: PathBuf,
}

/// Lowercase letters, digits, `-` and `_`, up to 40: safe in a URL path, a
/// file name and a `verified_by` value without escaping.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl Manifest {
    pub fn new(path: &Path) -> Self {
        Manifest {
            sectors: Vec::new(),
            default: None,
            dir: folder(path),
        }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut m: Manifest =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        m.dir = folder(path);
        m.check()?;
        Ok(m)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.check()?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Names are unique and valid; every `reads` entry exists and is not the
    /// sector itself. A bad manifest is refused whole, before anything serves.
    fn check(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for s in &self.sectors {
            if !valid_name(&s.name) {
                return Err(format!(
                    "sector name `{}`: use lowercase letters, digits, `-` or `_`, up to 40",
                    s.name
                ));
            }
            if !seen.insert(s.name.as_str()) {
                return Err(format!("sector `{}` is declared twice", s.name));
            }
        }
        for s in &self.sectors {
            for r in &s.reads {
                if r == &s.name {
                    return Err(format!("sector `{}` lists itself in `reads`", s.name));
                }
                if !seen.contains(r.as_str()) {
                    return Err(format!(
                        "sector `{}` reads `{r}`, which is not declared",
                        s.name
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&SectorDef> {
        self.sectors.iter().find(|s| s.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut SectorDef> {
        self.sectors.iter_mut().find(|s| s.name == name)
    }

    pub fn path_of(&self, s: &SectorDef) -> PathBuf {
        self.dir.join(&s.file)
    }

    /// The agent sector a key opens, if any. Compares digests in constant time.
    pub fn agent_for_key(&self, key: &str) -> Option<&SectorDef> {
        let digest = key_hash(key);
        self.sectors.iter().find(|s| {
            s.key_sha256
                .as_deref()
                .map_or(false, |k| constant_time_eq(k.as_bytes(), digest.as_bytes()))
        })
    }

    /// What an agent sector sees: itself first, then what it reads.
    pub fn view_of(&self, agent: &SectorDef) -> Vec<String> {
        std::iter::once(agent.name.clone())
            .chain(agent.reads.iter().cloned())
            .collect()
    }
}

fn folder(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// A fresh agent key: `fdsk_` and 32 hex digits from the OS's RNG-seeded hasher.
pub fn new_key() -> String {
    format!("fdsk_{}", crate::serve::new_token())
}

pub fn key_hash(key: &str) -> String {
    Sha256::digest(key.trim().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// One sector as a query sees it.
pub struct View<'a> {
    pub name: &'a str,
    pub engine: &'a Engine,
}

/// Queries every sector in `views` and merges the answers.
///
/// The overall confidence is the best any sector reached. Records are taken
/// by turns, one per sector at a time -- best sector first, the agent's own on
/// a tie -- because scores are not comparable across files: each has its own
/// word statistics. Links resolve inside their own sector.
pub async fn query(
    views: &[View<'_>],
    text: &str,
    scope: &Scope<'_>,
    budget: u32,
    links: bool,
) -> QueryResult {
    let mut vectors: HashMap<String, Option<Vec<f32>>> = HashMap::new();
    let mut per = Vec::with_capacity(views.len());
    for (order, v) in views.iter().enumerate() {
        let emb = if v.engine.has_embeddings() {
            let model = v.engine.meta.embedding_model.clone();
            if !vectors.contains_key(&model) {
                let e = ollama::embed_query(&model, text).await;
                vectors.insert(model.clone(), e);
            }
            vectors[&model].clone()
        } else {
            None
        };
        let mut r = v.engine.query_scoped(text, scope, budget, emb.as_deref());
        if links {
            v.engine.expand_links(&mut r, scope);
        }
        if views.len() > 1 {
            for h in &mut r.records {
                h.sector = Some(v.name.to_string());
            }
        }
        per.push((order, r));
    }
    merge(per, text, budget)
}

fn rank(c: Confidence) -> u8 {
    match c {
        Confidence::Exact => 0,
        Confidence::Strong => 1,
        Confidence::Weak => 2,
        Confidence::None => 3,
    }
}

fn merge(mut per: Vec<(usize, QueryResult)>, text: &str, budget: u32) -> QueryResult {
    if per.len() == 1 {
        return per.pop().map(|(_, r)| r).unwrap();
    }
    per.sort_by_key(|(order, r)| (rank(r.confidence), *order));
    let confidence = per
        .first()
        .map(|(_, r)| r.confidence)
        .unwrap_or(Confidence::None);
    let mut queues: Vec<VecDeque<_>> = per
        .into_iter()
        .map(|(_, r)| VecDeque::from(r.records))
        .collect();
    let (mut records, mut used) = (Vec::new(), 0u32);
    'fill: loop {
        let mut took = false;
        for q in queues.iter_mut() {
            if let Some(h) = q.pop_front() {
                if !records.is_empty() && used + h.tokens > budget {
                    break 'fill;
                }
                used += h.tokens;
                records.push(h);
                took = true;
            }
        }
        if !took {
            break;
        }
    }
    QueryResult {
        query: text.to_string(),
        confidence,
        records,
        used_tokens: used,
        budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fds::v4::DbMeta;

    fn engine_with(facts: &[(&str, &str)]) -> Engine {
        let mut records = Vec::new();
        for (q, a) in facts {
            crate::ops::upsert_fact(&mut records, q, a, "default", "test", "human", "2026-10-12")
                .unwrap();
        }
        Engine::new(DbMeta::default(), records)
    }

    #[test]
    fn names_are_checked_and_reads_must_exist() {
        let mut m = Manifest::new(Path::new("sectors.json"));
        m.sectors.push(SectorDef {
            name: "sales".into(),
            file: "sales.fds".into(),
            reads: vec!["shared".into()],
            key_sha256: None,
            clearance: vec![],
        });
        assert!(m.check().is_err(), "reads a sector that does not exist");
        m.sectors.push(SectorDef {
            name: "shared".into(),
            file: "shared.fds".into(),
            reads: vec![],
            key_sha256: None,
            clearance: vec![],
        });
        assert!(m.check().is_ok());
        m.sectors.push(SectorDef {
            name: "Bad Name".into(),
            file: "x.fds".into(),
            reads: vec![],
            key_sha256: None,
            clearance: vec![],
        });
        assert!(m.check().is_err());
    }

    #[test]
    fn a_key_opens_only_its_own_sector() {
        let mut m = Manifest::new(Path::new("sectors.json"));
        let (ka, kb) = (new_key(), new_key());
        m.sectors.push(SectorDef {
            name: "a".into(),
            file: "a.fds".into(),
            reads: vec![],
            key_sha256: Some(key_hash(&ka)),
            clearance: vec![],
        });
        m.sectors.push(SectorDef {
            name: "b".into(),
            file: "b.fds".into(),
            reads: vec![],
            key_sha256: Some(key_hash(&kb)),
            clearance: vec![],
        });
        assert_eq!(m.agent_for_key(&ka).unwrap().name, "a");
        assert_eq!(m.agent_for_key(&kb).unwrap().name, "b");
        assert!(m.agent_for_key("fdsk_guess").is_none());
        assert_ne!(ka, kb);
    }

    #[tokio::test]
    async fn the_best_sector_answers_first_and_each_record_says_where_it_lives() {
        let own = engine_with(&[("my note", "kept by the agent")]);
        let shared = engine_with(&[
            ("office wifi password", "ask reception"),
            ("printer on floor two", "HP 404"),
        ]);
        let views = [
            View {
                name: "sales",
                engine: &own,
            },
            View {
                name: "shared",
                engine: &shared,
            },
        ];
        let scope = Scope {
            collection: None,
            verified_by: &[],
        };
        let r = query(&views, "office wifi password", &scope, 400, false).await;
        assert_eq!(r.confidence, Confidence::Exact);
        assert_eq!(r.records[0].sector.as_deref(), Some("shared"));
        assert!(r.records[0].body.contains("reception"));
        let r = query(&views, "my note", &scope, 400, false).await;
        assert_eq!(r.records[0].sector.as_deref(), Some("sales"));
    }
}
