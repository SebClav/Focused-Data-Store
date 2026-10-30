//! Query evaluation (SPEC.md §4) and confidence (§5).
//!
//! Stage 1 matches compiled records (facts, procedures): exact, then
//! normalized, then -- only with embeddings -- semantic with a coverage gate.
//! Stage 2 ranks every record -- knowledge, plus compiled records as candidates
//! that are never `exact` -- with BM25 plus optional vectors, RRF-fused, packed
//! under a token budget. The result is records plus a confidence value; FDS
//! never generates text and never tells the caller what to do with it.

use std::collections::{BTreeMap, HashMap, HashSet};

use fds::v4::{DbMeta, Kind, Record};
use serde::Serialize;

use crate::text::{
    canonical, content_tokens, search_fold, search_terms, stable_id, token_set_key, wikilinks,
};

/// The one `type` FDS gives meaning to (SPEC §4.4). Every other type is the
/// owner's to define.
pub const CONTEXT_TYPE: &str = "context";

pub const DEFAULT_TAU: f32 = 0.60;
pub const DEFAULT_BUDGET: u32 = 400;
const TOP_K: usize = 5;
/// How many sections of one file a query by its path may return, within the budget.
const SOURCE_LIMIT: usize = 24;
const RRF_K: f32 = 60.0;
const BM25_K1: f32 = 1.5;
const BM25_B: f32 = 0.75;
const MIN_SHARED_TOKENS: usize = 2;
/// How many times the words of a record's title (and question) count against
/// one occurrence in its body. A heading names what its section is about, so a
/// query that is the heading should find the section, not only a shorter
/// section that happens to repeat the words.
const TITLE_WEIGHT: usize = 3;
/// A fact or procedure whose whole question is contained in the query is very
/// likely what was asked ("cafe menu items" for "What are the menu items?"), even
/// when the query adds a word that a longer, less relevant question happens to
/// share. Only a question contained *entirely* gets the boost: "pin 2 of the
/// NE555" must not promote the neighbouring fact about pin 4, whose question it
/// covers only in part. The score is multiplied by `1 + QUESTION_COVERAGE_BOOST`.
const QUESTION_COVERAGE_BOOST: f32 = 1.0;
/// A query term that no word of a record matches exactly still counts, at this
/// share of its weight, for a record word that is a form of it: `prices` for
/// `price`, `availability` for `available`. Stage 2 has no stemmer (the search
/// terms are normative, SPEC.md §4.1); this is a ranking choice of the reference
/// tool and never changes which terms a query has, so it cannot make a record
/// `strong` or `exact`.
const RELATED_TERM_WEIGHT: f32 = 0.5;

/// Whether two different terms are forms of one word: one is the other plus a
/// plural ending, or the shorter has at least eight letters and they share all but
/// its last two.
fn related_terms(a: &str, b: &str) -> bool {
    if a == b {
        return false;
    }
    let (short, long) = if a.chars().count() <= b.chars().count() { (a, b) } else { (b, a) };
    if short.chars().count() >= 3
        && (long == format!("{short}s") || long == format!("{short}es"))
    {
        return true;
    }
    let common = short.chars().zip(long.chars()).take_while(|(x, y)| x == y).count();
    let len = short.chars().count();
    len >= 8 && common >= len - 2
}

/// Floor of the length normalisation, as a share of the average record length.
const MIN_LEN_RATIO: f32 = 0.5;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Exact,
    Strong,
    Weak,
    None,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::Strong => "strong",
            Confidence::Weak => "weak",
            Confidence::None => "none",
        }
    }
}

/// A record as returned to callers: no embedding, plus how it matched.
#[derive(Serialize, Clone, Debug)]
pub struct Hit {
    pub id: String,
    pub kind: Kind,
    pub collection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    pub title: String,
    pub body: String,
    pub source: String,
    pub verified_by: String,
    pub last_verified: String,
    #[serde(rename = "match")]
    pub match_kind: String,
    /// The fused rank score. RRF ranks well but flattens magnitude, so it is
    /// a poor confidence signal on its own.
    pub score: f32,
    /// The raw retriever signals behind `score`, so a caller can calibrate its
    /// own threshold or train a reranker instead of trusting the fused rank.
    /// Zero means that retriever did not reach this record.
    pub bm25: f32,
    pub cosine: f32,
    #[serde(rename = "type", skip_serializing_if = "String::is_empty")]
    pub rtype: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: BTreeMap<String, String>,
    /// Where this record's links point. A link whose target does not exist
    /// yet has no `id`, as in Obsidian, and resolves once the target is added.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkRef>,
    /// The record's size, so answers merged from several sectors can keep
    /// to one budget.
    pub tokens: u32,
    /// The sector it came from, when the answer spans more than one (§10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sector: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LinkRef {
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The target's title as written, when it exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Which records a query may return (SPEC §4): a collection, and who
/// verified them. Applied identically at both stages and to linked records.
#[derive(Default, Clone, Copy)]
pub struct Scope<'a> {
    pub collection: Option<&'a str>,
    /// Accepted `verified_by` values; one ending in `:` is a prefix, so
    /// `model:` accepts every model. Empty accepts everything.
    pub verified_by: &'a [String],
}

impl<'a> Scope<'a> {
    #[cfg(test)]
    pub fn collection(collection: Option<&'a str>) -> Self {
        Scope {
            collection,
            verified_by: &[],
        }
    }

    fn admits(&self, r: &Record) -> bool {
        self.collection.map_or(true, |c| r.collection == c)
            && (self.verified_by.is_empty()
                || self.verified_by.iter().any(|v| {
                    if v.ends_with(':') {
                        r.verified_by.starts_with(v.as_str())
                    } else {
                        r.verified_by == *v
                    }
                }))
    }
}

/// Splits `human,model:` into the list a `Scope` takes.
pub fn verified_list(s: Option<&str>) -> Vec<String> {
    s.map(|s| {
        s.split(',')
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// A context loaded in one call (SPEC §4.4): its instructions and every
/// record it reaches through its links, with a hash of the lot so a caller
/// that already holds it can skip reading it again.
#[derive(Serialize, Debug)]
pub struct ContextBundle {
    pub id: String,
    pub name: String,
    pub collection: String,
    pub instructions: String,
    pub records: Vec<Hit>,
    /// Link targets that name no record yet.
    pub missing: Vec<String>,
    pub tokens: u32,
    pub hash: String,
    pub verified_by: String,
    pub last_verified: String,
}

#[derive(Serialize, Debug)]
pub struct QueryResult {
    pub query: String,
    pub confidence: Confidence,
    pub records: Vec<Hit>,
    pub used_tokens: u32,
    pub budget: u32,
}

pub struct Engine {
    pub meta: DbMeta,
    pub records: Vec<Record>,
    by_canonical: HashMap<String, usize>,
    by_token_set: HashMap<Vec<String>, usize>,
    compiled: Vec<usize>,
    /// Every record, for stage 2 ranking.
    searchable: Vec<usize>,
    /// Content tokens per record, aligned with `records`.
    tokens: Vec<Vec<String>>,
    doc_freq: HashMap<String, usize>,
    avg_len: f32,
    /// Outgoing links per record: canonical target, and the record it names
    /// if one exists.
    out_links: Vec<Vec<(String, Option<usize>)>>,
    /// Records that link to each record.
    backlinks: Vec<Vec<usize>>,
}

impl Engine {
    pub fn new(meta: DbMeta, records: Vec<Record>) -> Self {
        let mut by_canonical = HashMap::new();
        let mut by_token_set = HashMap::new();
        let mut compiled = Vec::new();

        for (i, r) in records.iter().enumerate() {
            if r.rtype == CONTEXT_TYPE {
                continue; // loaded by name, never matched as an answer
            }
            match r.kind {
                Kind::Fact | Kind::Procedure => {
                    if let Some(q) = &r.question {
                        by_canonical.entry(canonical(q)).or_insert(i);
                        by_token_set.entry(token_set_key(q)).or_insert(i);
                    }
                    compiled.push(i);
                }
                Kind::Knowledge => {}
            }
        }

        // Stage 2 searches every record. Compiled records that missed stage 1
        // compete as candidates -- otherwise a paraphrased intent could never
        // reach its procedure, which has no knowledge twin to fall back on.
        // They are indexed on question + title + body, since the question is
        // what a query resembles.
        let tokens: Vec<Vec<String>> = records.iter().map(index_terms).collect();
        let searchable: Vec<usize> = (0..records.len())
            .filter(|&i| records[i].rtype != CONTEXT_TYPE)
            .collect();
        let mut doc_freq: HashMap<String, usize> = HashMap::new();
        for &i in &searchable {
            for t in tokens[i].iter().collect::<HashSet<_>>() {
                *doc_freq.entry(t.clone()).or_insert(0) += 1;
            }
        }
        let avg_len = if searchable.is_empty() {
            1.0
        } else {
            searchable
                .iter()
                .map(|&i| tokens[i].len() as f32)
                .sum::<f32>()
                / searchable.len() as f32
        };

        // Links resolve by name, the way Obsidian resolves a note: a compiled
        // record by its question, any record by its title. Questions win a tie,
        // since they are what a fact or procedure is known by.
        let mut link_key: HashMap<String, usize> = HashMap::new();
        for (i, r) in records.iter().enumerate() {
            if let Some(q) = &r.question {
                link_key.entry(canonical(q)).or_insert(i);
            }
        }
        for (i, r) in records.iter().enumerate() {
            link_key.entry(canonical(&r.title)).or_insert(i);
        }
        let mut out_links = Vec::with_capacity(records.len());
        let mut backlinks = vec![Vec::new(); records.len()];
        for (i, r) in records.iter().enumerate() {
            let mut targets: Vec<String> = Vec::new();
            for t in r
                .links
                .iter()
                .map(|l| canonical(l))
                .chain(wikilinks(&r.body))
            {
                if !t.is_empty() && !targets.contains(&t) {
                    targets.push(t);
                }
            }
            let resolved: Vec<(String, Option<usize>)> = targets
                .into_iter()
                .map(|t| {
                    let to = link_key.get(&t).copied().filter(|&j| j != i);
                    (t, to)
                })
                .collect();
            for (_, to) in &resolved {
                if let Some(j) = to {
                    if !backlinks[*j].contains(&i) {
                        backlinks[*j].push(i);
                    }
                }
            }
            out_links.push(resolved);
        }

        Engine {
            meta,
            records,
            by_canonical,
            by_token_set,
            compiled,
            searchable,
            tokens,
            doc_freq,
            avg_len,
            out_links,
            backlinks,
        }
    }

    pub fn links_of(&self, i: usize) -> Vec<LinkRef> {
        self.out_links[i]
            .iter()
            .map(|(t, to)| LinkRef {
                target: t.clone(),
                id: to.map(|j| self.records[j].id.clone()),
                title: to.map(|j| self.records[j].title.clone()),
            })
            .collect()
    }

    pub fn backlinks_of(&self, i: usize) -> &[usize] {
        &self.backlinks[i]
    }

    /// Every resolved link as (from, to) record indexes: the graph.
    pub fn edges(&self) -> Vec<(usize, usize)> {
        self.out_links
            .iter()
            .enumerate()
            .flat_map(|(i, ls)| ls.iter().filter_map(move |(_, to)| to.map(|j| (i, j))))
            .collect()
    }

    /// Adds the records that the result's records link to, one hop out, as
    /// `link` matches after the ranked ones and within the remaining budget.
    /// Confidence is not touched: a linked record is context, not evidence.
    pub fn expand_links(&self, result: &mut QueryResult, scope: &Scope) {
        let mut have: HashSet<String> = result.records.iter().map(|h| h.id.clone()).collect();
        let by_id: HashMap<&str, usize> = self
            .records
            .iter()
            .enumerate()
            .map(|(i, r)| (r.id.as_str(), i))
            .collect();
        let sources: Vec<usize> = result
            .records
            .iter()
            .filter_map(|h| by_id.get(h.id.as_str()).copied())
            .collect();
        for i in sources {
            for (_, to) in &self.out_links[i] {
                let Some(j) = *to else { continue };
                let r = &self.records[j];
                if r.rtype == CONTEXT_TYPE || have.contains(&r.id) || !scope.admits(r) {
                    continue;
                }
                if result.used_tokens + r.tokens > result.budget {
                    continue;
                }
                have.insert(r.id.clone());
                result.used_tokens += r.tokens;
                result.records.push(self.hit(j, "link", 0.0));
            }
        }
    }

    pub fn contexts(&self) -> Vec<usize> {
        (0..self.records.len())
            .filter(|&i| self.records[i].rtype == CONTEXT_TYPE)
            .collect()
    }

    /// A context by name (its title, compared canonically) or by id or id
    /// prefix of six or more characters.
    pub fn find_context(&self, wanted: &str) -> Option<usize> {
        let key = canonical(wanted);
        let ctx = self.contexts();
        ctx.iter()
            .copied()
            .find(|&i| canonical(&self.records[i].title) == key)
            .or_else(|| ctx.iter().copied().find(|&i| self.records[i].id == wanted))
            .or_else(|| {
                let hits: Vec<usize> = ctx
                    .iter()
                    .copied()
                    .filter(|&i| wanted.len() >= 6 && self.records[i].id.starts_with(wanted))
                    .collect();
                (hits.len() == 1).then(|| hits[0])
            })
    }

    /// Loads a context: its instructions plus everything its links reach,
    /// depth-first in link order. A linked context contributes its own
    /// instructions and members; cycles are followed once.
    pub fn context(&self, i: usize) -> ContextBundle {
        let mut seen: HashSet<usize> = HashSet::from([i]);
        let mut records = Vec::new();
        let mut missing = Vec::new();
        self.walk_context(i, &mut seen, &mut records, &mut missing);

        let c = &self.records[i];
        let tokens = c.tokens
            + records
                .iter()
                .map(|h: &Hit| crate::text::word_count(&h.body))
                .sum::<u32>();
        // Content only: re-verifying a record without changing it must not
        // make every agent read the context again.
        let mut fingerprint = format!("{}\u{1f}{}\u{1f}{}", c.id, c.title, c.body);
        for h in &records {
            fingerprint.push_str(&format!(
                "\u{1e}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{:?}",
                h.id, h.title, h.body, h.rtype, h.attrs
            ));
        }
        for m in &missing {
            fingerprint.push_str(&format!("\u{1d}{m}"));
        }
        ContextBundle {
            id: c.id.clone(),
            name: c.title.clone(),
            collection: c.collection.clone(),
            instructions: c.body.clone(),
            records,
            missing,
            tokens,
            hash: stable_id(&fingerprint),
            verified_by: c.verified_by.clone(),
            last_verified: c.last_verified.clone(),
        }
    }

    fn walk_context(
        &self,
        i: usize,
        seen: &mut HashSet<usize>,
        out: &mut Vec<Hit>,
        missing: &mut Vec<String>,
    ) {
        for (target, to) in &self.out_links[i] {
            match to {
                None => {
                    if !missing.contains(target) {
                        missing.push(target.clone());
                    }
                }
                Some(j) if seen.insert(*j) => {
                    let nested = self.records[*j].rtype == CONTEXT_TYPE;
                    out.push(self.hit(*j, if nested { "context" } else { "member" }, 0.0));
                    if nested {
                        self.walk_context(*j, seen, out, missing);
                    }
                }
                Some(_) => {}
            }
        }
    }

    pub fn has_embeddings(&self) -> bool {
        !self.meta.embedding_model.is_empty() && self.records.iter().any(|r| r.embedding.is_some())
    }

    fn in_scope(&self, i: usize, scope: &Scope) -> bool {
        scope.admits(&self.records[i])
    }

    fn hit(&self, i: usize, match_kind: &str, score: f32) -> Hit {
        let r = &self.records[i];
        Hit {
            id: r.id.clone(),
            kind: r.kind,
            collection: r.collection.clone(),
            question: r.question.clone(),
            title: r.title.clone(),
            body: r.body.clone(),
            source: r.source.clone(),
            verified_by: r.verified_by.clone(),
            last_verified: r.last_verified.clone(),
            match_kind: match_kind.to_string(),
            score: (score * 10_000.0).round() / 10_000.0,
            bm25: 0.0,
            cosine: 0.0,
            rtype: r.rtype.clone(),
            attrs: r.attrs.clone(),
            links: self.links_of(i),
            tokens: r.tokens,
            sector: None,
        }
    }

    /// Stage 1 (§4.2). Precision over recall: a miss falls through to
    /// knowledge retrieval, a false match returns a wrong compiled answer.
    fn match_compiled(&self, query: &str, scope: &Scope, q_emb: Option<&[f32]>) -> Option<Hit> {
        if let Some(&i) = self.by_canonical.get(&canonical(query)) {
            if self.in_scope(i, scope) {
                return Some(self.hit(i, "exact", 1.0));
            }
        }
        if let Some(&i) = self.by_token_set.get(&token_set_key(query)) {
            if self.in_scope(i, scope) {
                return Some(self.hit(i, "normalized", 1.0));
            }
        }

        let q_emb = q_emb?;
        let q_tokens: HashSet<String> = content_tokens(query).into_iter().collect();
        let mut best: Option<(usize, f32)> = None;
        for &i in &self.compiled {
            if !self.in_scope(i, scope) {
                continue;
            }
            if let Some(e) = &self.records[i].embedding {
                let s = cosine(q_emb, e);
                if best.map_or(true, |(_, b)| s > b) {
                    best = Some((i, s));
                }
            }
        }
        let (i, score) = best?;
        if score < DEFAULT_TAU {
            return None;
        }
        // Coverage gate (REQUIRED by §4.2): every content token of the
        // compiled question must appear in the query.
        let r_tokens: HashSet<String> = self.records[i]
            .question
            .as_deref()
            .map(content_tokens)
            .unwrap_or_default()
            .into_iter()
            .collect();
        if r_tokens.intersection(&q_tokens).count() < MIN_SHARED_TOKENS
            || !r_tokens.is_subset(&q_tokens)
        {
            return None;
        }
        Some(self.hit(i, "semantic", score))
    }

    fn bm25(&self, query: &str, allowed: &[usize]) -> Vec<(usize, f32)> {
        self.bm25_top(query, allowed, TOP_K)
    }

    fn bm25_top(&self, query: &str, allowed: &[usize], limit: usize) -> Vec<(usize, f32)> {
        let q = search_terms(query);
        let q_set: HashSet<&String> = q.iter().collect();
        let n = self.searchable.len() as f32;
        let mut scored: Vec<(usize, f32)> = allowed
            .iter()
            .filter_map(|&i| {
                let doc = &self.tokens[i];
                // A section of nine words is not nine times more relevant than one of
                // ninety: its length counts as at least half of the average.
                let len = (doc.len() as f32).max(MIN_LEN_RATIO * self.avg_len);
                let mut s = 0.0f32;
                for qt in &q {
                    let mut tf = doc.iter().filter(|t| *t == qt).count() as f32;
                    let mut weight = 1.0;
                    if tf == 0.0 {
                        tf = doc.iter().filter(|t| related_terms(qt, t)).count() as f32;
                        weight = RELATED_TERM_WEIGHT;
                    }
                    if tf == 0.0 {
                        continue;
                    }
                    let df = *self.doc_freq.get(qt).unwrap_or(&0) as f32;
                    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                    s += weight * idf * (tf * (BM25_K1 + 1.0))
                        / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * len / self.avg_len));
                }
                if s > 0.0 {
                    if let Some(question) = &self.records[i].question {
                        let terms: HashSet<String> = search_terms(question).into_iter().collect();
                        if !terms.is_empty() && terms.iter().all(|t| q_set.contains(t)) {
                            s *= 1.0 + QUESTION_COVERAGE_BOOST;
                        }
                    }
                }
                (s > 0.0).then_some((i, s))
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(limit);
        scored
    }

    /// A query that is nothing but a file name or path (SPEC.md §4.3): the
    /// records, among `allowed`, whose `source` it names, in the order they are
    /// stored. It names a source when it is that source's path, or the last
    /// segments of it and no other source ends the same way. Agents read the
    /// documents of a project by path, so asking for one by path must work.
    fn source_lookup(&self, query: &str, allowed: &[usize]) -> Option<Vec<usize>> {
        let q = query
            .trim()
            .trim_matches(|c: char| {
                matches!(
                    c,
                    '`' | '"' | '\'' | '(' | ')' | '<' | '>' | ',' | ';' | ':'
                )
            })
            .replace('\\', "/");
        let q = search_fold(q.trim_start_matches("./"));
        if q.is_empty() || q.contains(char::is_whitespace) || !(q.contains('/') || q.contains('.'))
        {
            return None;
        }
        let mut exact: Vec<&str> = Vec::new();
        let mut ending: Vec<&str> = Vec::new();
        for &i in allowed {
            let src = self.records[i].source.as_str();
            let low = search_fold(src);
            if low == q {
                if !exact.contains(&src) {
                    exact.push(src);
                }
            } else if low.ends_with(&format!("/{q}")) && !ending.contains(&src) {
                ending.push(src);
            }
        }
        let wanted = if !exact.is_empty() {
            exact
        } else if ending.len() == 1 {
            ending
        } else {
            return None;
        };
        let ids: Vec<usize> = allowed
            .iter()
            .copied()
            .filter(|&i| wanted.contains(&self.records[i].source.as_str()))
            .collect();
        (!ids.is_empty()).then_some(ids)
    }

    fn vectors(&self, q_emb: &[f32], allowed: &[usize]) -> Vec<(usize, f32)> {
        let mut scored: Vec<(usize, f32)> = allowed
            .iter()
            .filter_map(|&i| {
                self.records[i]
                    .embedding
                    .as_ref()
                    .map(|e| (i, cosine(q_emb, e)))
            })
            .filter(|(_, s)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(TOP_K);
        scored
    }

    /// The unscoped query, for tests; everything else passes a `Scope`.
    #[cfg(test)]
    pub fn query(
        &self,
        text: &str,
        collection: Option<&str>,
        budget: u32,
        q_emb: Option<&[f32]>,
    ) -> QueryResult {
        self.query_scoped(text, &Scope::collection(collection), budget, q_emb)
    }

    pub fn query_scoped(
        &self,
        text: &str,
        scope: &Scope,
        budget: u32,
        q_emb: Option<&[f32]>,
    ) -> QueryResult {
        if let Some(hit) = self.match_compiled(text, scope, q_emb) {
            let used = crate::text::word_count(&hit.body);
            return QueryResult {
                query: text.to_string(),
                confidence: Confidence::Exact,
                records: vec![hit],
                used_tokens: used,
                budget,
            };
        }

        // Stage 2 (§4.3). Collection filter before ranking.
        let allowed: Vec<usize> = self
            .searchable
            .iter()
            .copied()
            .filter(|&i| self.in_scope(i, scope))
            .collect();

        let by_path = self.source_lookup(text, &allowed);
        let keyword = match &by_path {
            Some(ids) => {
                let mut k = self.bm25_top(text, ids, usize::MAX);
                k.sort_by_key(|(i, _)| *i);
                k.truncate(SOURCE_LIMIT);
                k
            }
            None => self.bm25(text, &allowed),
        };

        // The keyword gate. Vectors always find something that looks similar,
        // so on their own they can never answer `none` -- measured: 10/10
        // out-of-domain queries came back with records, all vector-only.
        // BM25 returning nothing means the query shares no content word with
        // any record, and that is the honest signal for "not in this database".
        // Vectors still rank and still pull in records BM25 missed; they just
        // no longer decide whether there is an answer at all.
        if keyword.is_empty() {
            return QueryResult {
                query: text.to_string(),
                confidence: Confidence::None,
                records: Vec::new(),
                used_tokens: 0,
                budget,
            };
        }

        // A file asked for by its path is read in order, not by similarity.
        let vector = match q_emb {
            Some(e) if by_path.is_none() => self.vectors(e, &allowed),
            _ => Vec::new(),
        };

        let raw_kw: HashMap<usize, f32> = keyword.iter().copied().collect();
        let raw_vec: HashMap<usize, f32> = vector.iter().copied().collect();

        let mut fused: HashMap<usize, (f32, bool, bool)> = HashMap::new();
        for (rank, (i, _)) in keyword.iter().enumerate() {
            let e = fused.entry(*i).or_insert((0.0, false, false));
            e.0 += 1.0 / (RRF_K + rank as f32 + 1.0);
            e.1 = true;
        }
        for (rank, (i, _)) in vector.iter().enumerate() {
            let e = fused.entry(*i).or_insert((0.0, false, false));
            e.0 += 1.0 / (RRF_K + rank as f32 + 1.0);
            e.2 = true;
        }
        let mut ranked: Vec<(usize, f32, bool, bool)> = fused
            .into_iter()
            .map(|(i, (s, k, v))| (i, s, k, v))
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

        let mut hits = Vec::new();
        let mut used = 0u32;
        for (i, score, by_kw, by_vec) in ranked {
            let t = self.records[i].tokens;
            if used + t > budget && !hits.is_empty() {
                continue;
            }
            let m = match (by_kw, by_vec) {
                (true, true) => "keyword+vector",
                (true, false) => "keyword",
                _ => "vector",
            };
            let mut h = self.hit(i, m, score);
            h.bm25 = (raw_kw.get(&i).copied().unwrap_or(0.0) * 10_000.0).round() / 10_000.0;
            h.cosine = (raw_vec.get(&i).copied().unwrap_or(0.0) * 10_000.0).round() / 10_000.0;
            hits.push((i, h));
            used += t;
            if used >= budget {
                break;
            }
        }

        // §5: strong = the FIRST record contains every content token of the
        // query. A structural condition, not a tuned threshold. Agreement of
        // the two retrievers is not enough: with vectors on, some record
        // nearly always has both signals, and 16-28% of those answers were
        // wrong across four domains (measured by the author).
        let q_tokens: HashSet<String> = search_terms(text).into_iter().collect();
        let covered = !q_tokens.is_empty()
            && hits.first().map_or(false, |(i, _)| {
                let rt: HashSet<&String> = self.tokens[*i].iter().collect();
                q_tokens.iter().all(|t| rt.contains(t))
            });

        let confidence = if hits.is_empty() {
            Confidence::None
        } else if covered {
            Confidence::Strong
        } else {
            Confidence::Weak
        };

        QueryResult {
            query: text.to_string(),
            confidence,
            records: hits.into_iter().map(|(_, h)| h).collect(),
            used_tokens: used,
            budget,
        }
    }
}

/// What stage 2 indexes for a record (SPEC.md §4.3): the search terms of its
/// body; of its title and question, which describe the record and count
/// `TITLE_WEIGHT` times (for a section the title is the heading path, and only
/// its last heading counts that much: the headings above it name the whole
/// document and are the same in every section of it); and of its source path.
/// Facts and procedures are indexed on their question too, since that is what
/// a query resembles.
fn index_terms(r: &Record) -> Vec<String> {
    let mut terms = search_terms(&r.body);
    let (above, own) = match r.title.rsplit_once(" > ") {
        Some((above, leaf)) => (above, leaf),
        None => ("", r.title.as_str()),
    };
    let own = match &r.question {
        Some(q) if canonical(own) != canonical(q) => format!("{q} {own}"),
        Some(q) => q.clone(),
        None => own.to_string(),
    };
    let own = search_terms(&own);
    for _ in 0..TITLE_WEIGHT {
        terms.extend(own.iter().cloned());
    }
    terms.extend(search_terms(above));
    terms.extend(search_terms(&r.source));
    terms
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(q: &str, a: &str) -> Record {
        Record {
            id: crate::text::stable_id(&format!("fact:{}", canonical(q))),
            kind: Kind::Fact,
            collection: "default".into(),
            question: Some(canonical(q)),
            body: a.into(),
            title: q.into(),
            source: "manual".into(),
            verified_by: "human".into(),
            last_verified: "2026-09-17".into(),
            tokens: crate::text::word_count(a),
            embedding: None,
            rtype: String::new(),
            attrs: Default::default(),
            links: Vec::new(),
        }
    }

    fn knowledge(id: &str, body: &str) -> Record {
        Record {
            id: id.into(),
            kind: Kind::Knowledge,
            collection: "default".into(),
            question: None,
            body: body.into(),
            title: id.into(),
            source: "doc.md".into(),
            verified_by: "ingest".into(),
            last_verified: "2026-09-17".into(),
            tokens: crate::text::word_count(body),
            embedding: None,
            rtype: String::new(),
            attrs: Default::default(),
            links: Vec::new(),
        }
    }

    fn engine() -> Engine {
        Engine::new(
            DbMeta::default(),
            vec![
                fact("what is pin 4 of the ne555", "RESET"),
                fact("what is the maximum output current of the lm317", "1.5 A"),
                knowledge("k1", "Pin 2 of the NE555 is TRIG, the trigger input."),
                knowledge(
                    "k2",
                    "The LM317 needs a minimum load current of 10 mA to regulate.",
                ),
            ],
        )
    }

    #[test]
    fn a_question_the_query_contains_outranks_a_longer_one_sharing_a_word() {
        let e = Engine::new(
            DbMeta::default(),
            vec![
                fact("what are the cafe menu items draft", "model proposal"),
                fact("what is the cafe visual identity", "colors"),
                fact("what is the menu items for lantern house", "other cafe"),
                fact("what are the menu items", "the real menu"),
            ],
        );
        for q in ["cafe menu items", "cafe menu items prices availability"] {
            let r = e.query(q, None, 400, None);
            assert_eq!(r.records[0].body, "the real menu", "{q}");
        }
    }

    #[test]
    fn related_forms_match_at_a_lower_weight() {
        assert!(related_terms("prices", "price"));
        assert!(related_terms("availability", "available"));
        assert!(related_terms("items", "item"));
        assert!(!related_terms("price", "price"));
        assert!(!related_terms("menu", "mental"));
        assert!(!related_terms("cafe", "cafes_x"));
        let e = Engine::new(
            DbMeta::default(),
            vec![
                fact("what are the opening hours", "9 to 5"),
                fact("what is the refund policy", "30 days"),
            ],
        );
        let r = e.query("refunds", None, 400, None);
        assert_eq!(r.records[0].body, "30 days");
        // A related form alone never makes a record strong.
        assert_ne!(r.confidence, Confidence::Strong);
    }

    #[test]
    fn exact_match_short_circuits() {
        let r = engine().query("What is pin 4 of the NE555?", None, 400, None);
        assert_eq!(r.confidence, Confidence::Exact);
        assert_eq!(r.records[0].body, "RESET");
        assert_eq!(r.records[0].match_kind, "exact");
    }

    #[test]
    fn normalized_match_ignores_order_and_stopwords() {
        let r = engine().query("NE555 pin 4?", None, 400, None);
        assert_eq!(r.confidence, Confidence::Exact);
        assert_eq!(r.records[0].match_kind, "normalized");
    }

    #[test]
    fn near_miss_questions_do_not_short_circuit() {
        // The electronics-test failures: must fall through, not return the
        // neighbouring compiled fact.
        let r = engine().query("what is pin 2 of the ne555", None, 400, None);
        assert_ne!(r.confidence, Confidence::Exact);
        assert!(r.records[0].body.contains("TRIG"));

        let r = engine().query(
            "what minimum load current does the lm317 need",
            None,
            400,
            None,
        );
        assert_ne!(r.confidence, Confidence::Exact);
        assert!(r.records[0].body.contains("10 mA"));
    }

    #[test]
    fn off_topic_query_is_weak_or_none() {
        let r = engine().query("airspeed velocity of an unladen swallow", None, 400, None);
        assert!(matches!(r.confidence, Confidence::Weak | Confidence::None));
    }

    #[test]
    fn a_query_sharing_no_word_is_none_and_returns_nothing() {
        // The keyword gate. Without it, vectors hand back the nearest record
        // for any query at all and `none` can never fire -- measured on 10
        // out-of-domain queries, all of which came back with records.
        let r = engine().query("what is the capital of paraguay", None, 400, None);
        assert_eq!(r.confidence, Confidence::None);
        assert!(r.records.is_empty(), "none must carry no records");
    }

    #[test]
    fn hits_carry_the_raw_retriever_scores() {
        // Callers calibrate on these; the fused score alone cannot separate
        // a right answer from a wrong one (§8).
        let r = engine().query("lm317 load current", None, 400, None);
        let h = r.records.first().expect("expected a candidate");
        assert!(h.bm25 > 0.0, "bm25 must be exported, got {}", h.bm25);
        assert_eq!(h.cosine, 0.0, "no query embedding was given");
    }

    #[test]
    fn full_coverage_is_strong() {
        let r = engine().query("lm317 minimum load current", None, 400, None);
        assert_eq!(r.confidence, Confidence::Strong);
    }

    #[test]
    fn paraphrased_intent_reaches_procedure_as_candidate() {
        let mut e = engine().records;
        e.push(Record {
            id: "p1".into(),
            kind: Kind::Procedure,
            question: Some(canonical("register a new customer")),
            title: "Customer registration form".into(),
            body: r##"{"steps":[{"action":"click","selector":"#save"}]}"##.into(),
            ..e[0].clone()
        });
        let r = Engine::new(DbMeta::default(), e).query(
            "how do I register a customer",
            None,
            400,
            None,
        );
        assert_eq!(r.records[0].kind, Kind::Procedure);
        // Found, but never promoted to exact: the caller decides whether to run it.
        assert_ne!(r.confidence, Confidence::Exact);
        assert_eq!(r.records[0].match_kind, "keyword");
    }

    fn context(name: &str, body: &str) -> Record {
        Record {
            rtype: CONTEXT_TYPE.into(),
            title: name.into(),
            ..knowledge(&format!("ctx-{}", canonical(name)), body)
        }
    }

    fn graph() -> Engine {
        let mut reboot = fact(
            "how do I reboot the router",
            "Hold RESET for 10 s. Check [[router admin ip]] first.",
        );
        reboot.links = vec!["Router back panel".into()];
        Engine::new(
            DbMeta::default(),
            vec![
                reboot,
                fact("router admin ip", "192.168.0.1"),
                Record { title: "Router back panel".into(), rtype: "image".into(), ..knowledge("img1", "4 yellow LAN ports, blue WAN, recessed RESET.") },
                context("Network support", "Answer in Spanish. Never promise a visit.\n[[how do I reboot the router]]\n[[Billing]]\n[[printer drivers]]"),
                context("Billing", "Refunds over 100 EUR go to a person. [[Network support]]"),
            ],
        )
    }

    #[test]
    fn links_resolve_by_question_or_title_and_record_backlinks() {
        let e = graph();
        let links = e.links_of(0);
        let targets: Vec<(&str, bool)> = links
            .iter()
            .map(|l| (l.target.as_str(), l.id.is_some()))
            .collect();
        // Explicit links first, then the ones written in the body.
        assert_eq!(
            targets,
            vec![("router back panel", true), ("router admin ip", true)]
        );
        assert_eq!(
            e.backlinks_of(1),
            &[0],
            "the admin-ip fact knows who points at it"
        );
        assert!(e.edges().contains(&(0, 2)));
    }

    #[test]
    fn contexts_are_loaded_by_name_never_returned_as_answers() {
        let e = graph();
        let r = e.query("refunds over 100 EUR", None, 400, None);
        assert!(
            r.records.iter().all(|h| h.rtype != CONTEXT_TYPE),
            "a context is instructions, not an answer"
        );
        assert!(
            e.find_context("network SUPPORT").is_some(),
            "found by name, canonically"
        );
        assert!(e.find_context("no such context").is_none());
    }

    #[test]
    fn a_context_bundles_its_links_follows_nested_ones_once_and_names_what_is_missing() {
        let e = graph();
        let b = e.context(e.find_context("Network support").unwrap());
        assert!(b.instructions.starts_with("Answer in Spanish"));
        let got: Vec<(&str, &str)> = b
            .records
            .iter()
            .map(|h| (h.title.as_str(), h.match_kind.as_str()))
            .collect();
        // Billing links back to Network support: the cycle is followed once.
        assert_eq!(
            got,
            vec![
                ("how do I reboot the router", "member"),
                ("Billing", "context")
            ]
        );
        assert_eq!(b.missing, vec!["printer drivers".to_string()]);
        assert!(b.tokens > 0);
    }

    #[test]
    fn the_context_hash_ignores_dates_and_notices_content() {
        let base = graph();
        let i = base.find_context("Network support").unwrap();
        let h0 = base.context(i).hash;

        let mut recs = base.records.clone();
        recs[0].last_verified = "2030-01-01".into(); // re-verified, unchanged
        assert_eq!(
            Engine::new(DbMeta::default(), recs.clone()).context(i).hash,
            h0
        );

        recs[0].body = "Hold RESET for 30 s.".into(); // a member changed
        assert_ne!(Engine::new(DbMeta::default(), recs).context(i).hash, h0);
    }

    #[test]
    fn expanding_links_adds_one_hop_within_budget_and_leaves_confidence_alone() {
        let e = graph();
        let mut r = e.query("how do I reboot the router", None, 400, None);
        let before = r.confidence;
        e.expand_links(&mut r, &Scope::default());
        let kinds: Vec<(&str, &str)> = r
            .records
            .iter()
            .map(|h| (h.title.as_str(), h.match_kind.as_str()))
            .collect();
        assert_eq!(kinds[0].1, "exact");
        assert!(
            kinds.contains(&("router admin ip", "link"))
                && kinds.contains(&("Router back panel", "link"))
        );
        assert_eq!(r.confidence, before);

        let mut tight = e.query("how do I reboot the router", None, 1, None);
        e.expand_links(&mut tight, &Scope::default());
        assert_eq!(tight.records.len(), 1, "no budget left, nothing added");
    }

    #[test]
    fn a_verified_by_scope_holds_at_both_stages_and_for_links() {
        let mut model_ip = fact("router admin ip", "192.168.0.1");
        model_ip.verified_by = "model:qwen2.5vl:3b".into();
        let mut reboot = fact(
            "how do I reboot the router",
            "Hold RESET. See [[router admin ip]].",
        );
        reboot.verified_by = "human".into();
        let e = Engine::new(DbMeta::default(), vec![model_ip, reboot]);
        let human = vec!["human".to_string()];
        let only_human = Scope {
            collection: None,
            verified_by: &human,
        };

        // Stage 1 would match exactly, but the record is out of scope.
        let r = e.query_scoped("router admin ip", &only_human, 400, None);
        assert!(
            r.records.iter().all(|h| h.verified_by == "human"),
            "{:?}",
            r.records
        );

        // A link does not smuggle in what the scope excludes.
        let mut r = e.query_scoped("how do I reboot the router", &only_human, 400, None);
        e.expand_links(&mut r, &only_human);
        assert_eq!(r.records.len(), 1);

        // `model:` is a prefix.
        let models = vec!["model:".to_string()];
        let r = e.query_scoped(
            "router admin ip",
            &Scope {
                collection: None,
                verified_by: &models,
            },
            400,
            None,
        );
        assert_eq!(
            (r.confidence, r.records[0].verified_by.as_str()),
            (Confidence::Exact, "model:qwen2.5vl:3b")
        );
    }

    fn section(id: &str, source: &str, title: &str, body: &str) -> Record {
        Record {
            title: title.into(),
            source: source.into(),
            ..knowledge(id, body)
        }
    }

    /// A small manual: sections whose bodies never say the word of their
    /// heading, a one-line intro that only points at the table, and a footer
    /// that repeats a common word.
    fn manual() -> Engine {
        Engine::new(
            DbMeta::default(),
            vec![
                section("a", "docs/negocio.md", "Datos del negocio > Horario", "Martes a sábado: 17:00 a 01:00. Domingo: 17:00 a 23:00. Lunes cerrado. La cocina cierra una hora antes que la barra."),
                section("b", "docs/negocio.md", "Datos del negocio > Contacto", "WhatsApp del local: +591 70000000. Correo: hola@terraza.example."),
                section("c", "docs/diseno.md", "Sistema de diseño", "Los valores viven en el archivo de variables CSS."),
                section("d", "docs/diseno.md", "Sistema de diseño > Nombres de las variables", "| Variable | Valor |\n|---|---|\n| `--crema` | fondo |\n| `--cafe` | texto |\n| `--naranja` | botón |\n| `--borde` | líneas |"),
                section("e", "docs/pie.md", "Pie de página", "Consulta el horario completo en la sección de arriba. Derechos reservados. Terraza Mirador, La Paz."),
            ],
        )
    }

    #[test]
    fn a_query_without_accents_finds_the_text_that_has_them() {
        let e = Engine::new(
            DbMeta::default(),
            vec![
                knowledge("k1", "El horario de atención es de martes a sábado."),
                knowledge("k2", "La cocina cierra a las 23:00."),
            ],
        );
        for q in [
            "horario de atencion",
            "horario de atención",
            "HORARIO DE ATENCIÓN",
            "horario de atenci\u{0301}on",
        ] {
            let r = e.query(q, None, 400, None);
            assert_eq!(r.confidence, Confidence::Strong, "{q}");
            assert_eq!(r.records[0].id, "k1", "{q}");
        }
        // And the other way round: a text without accents, a query with them.
        let e = Engine::new(
            DbMeta::default(),
            vec![knowledge(
                "k1",
                "El horario de atencion es de martes a sabado.",
            )],
        );
        assert_eq!(
            e.query("¿Cuál es el horario de atención?", None, 400, None)
                .records[0]
                .id,
            "k1"
        );
    }

    #[test]
    fn any_script_is_found_by_its_folded_spelling() {
        let docs = [
            ("de", "Wir wohnen in der Straße am Fluss."),
            ("el", "Η οδός Ερμού είναι στην Αθήνα."),
            ("tr", "İstanbul boğazı çok güzel."),
            ("ru", "Столица России — Москва, ёлка на площади."),
            ("ar", "مَرْحَبًا بكم في المقهى"),
            ("he", "שָׁלוֹם עולם"),
            ("hi", "नमस्ते दुनिया"),
            ("th", "ร้านกาแฟที่ลาปาซ"),
            ("zh", "我们的咖啡馆在拉巴斯"),
            ("ja", "東京タワーの近くのカフェ"),
            ("ko", "서울시 한국어 학원"),
            ("fw", "ＣＡＦＥ ＬＡ ＰＡＺ"),
            ("es", "Café con leche y pastel de choclo."),
            ("emoji", "🍕🎉 fiesta 🥳"),
        ];
        let e = Engine::new(
            DbMeta::default(),
            docs.iter().map(|(id, b)| knowledge(id, b)).collect(),
        );
        let first = |q: &str| {
            let r = e.query(q, None, 400, None);
            (
                r.records.first().map(|h| h.id.clone()).unwrap_or_default(),
                r.confidence,
            )
        };
        for (q, id) in [
            ("strasse", "de"),
            ("STRASSE", "de"),
            ("οδος ερμου", "el"),
            ("ΟΔΟΣ", "el"),
            ("istanbul", "tr"),
            ("bogazi", "tr"),
            ("москва", "ru"),
            ("елка", "ru"),
            ("مرحبا", "ar"),
            ("שלום", "he"),
            ("नमस्ते", "hi"),
            ("กาแฟ", "th"),
            ("咖啡", "zh"),
            ("拉巴斯", "zh"),
            ("東京タワー", "ja"),
            ("서울", "ko"),
            ("한국어", "ko"),
            ("cafe", "es"),
            ("ca\u{0066}\u{0065}\u{0301} con leche", "es"),
            ("café", "fw"),
            ("fiesta", "emoji"),
        ] {
            let (got, conf) = first(q);
            if q == "café" {
                // "cafe" is both a Spanish word and a full-width one; either is right.
                assert!(got == "es" || got == "fw", "{q}");
            } else {
                assert_eq!(got, id, "{q}");
                assert!(matches!(conf, Confidence::Strong | Confidence::Weak), "{q}");
            }
        }
        // Symbols alone are not terms: nothing to match, and nothing crashes.
        assert_eq!(e.query("🍕", None, 400, None).confidence, Confidence::None);
        assert_eq!(
            e.query("\u{301}\u{0}\u{FFFF}", None, 400, None).confidence,
            Confidence::None
        );
        // A query that is not in the database stays unanswered.
        assert_eq!(
            e.query("咖茶", None, 400, None).confidence,
            Confidence::Weak
        );
    }

    #[test]
    fn the_accent_fold_does_not_touch_stage_one_or_the_stored_text() {
        let e = Engine::new(
            DbMeta::default(),
            vec![fact("horario de atención", "Mar a sáb 17:00 a 01:00")],
        );
        let r = e.query("horario de atención", None, 400, None);
        assert_eq!(
            (r.confidence, r.records[0].match_kind.as_str()),
            (Confidence::Exact, "exact")
        );
        // Without the accent it is not a compiled match (precision first), but
        // the fact is the first candidate of stage 2 and covers the query.
        let r = e.query("horario de atencion", None, 400, None);
        assert_eq!(
            (r.confidence, r.records[0].match_kind.as_str()),
            (Confidence::Strong, "keyword")
        );
        assert_eq!(r.records[0].body, "Mar a sáb 17:00 a 01:00");
        assert_eq!(r.records[0].title, "horario de atención");
    }

    #[test]
    fn a_heading_names_its_section() {
        let e = manual();
        // The answer never says "horario"; its heading does. The footer says it once, in a short body.
        let ids = |r: &QueryResult| r.records.iter().map(|h| h.id.clone()).collect::<Vec<_>>();
        let r = e.query("horario", None, 400, None);
        assert_eq!(r.records[0].id, "a", "{:?}", ids(&r));
        let r = e.query("horario de atencion", None, 400, None);
        assert_eq!(r.records[0].id, "a", "{:?}", ids(&r));
        // The table has the specific heading; the one-line intro only points at it.
        let r = e.query("nombres de las variables", None, 400, None);
        assert_eq!(r.records[0].id, "d", "{:?}", ids(&r));
        let r = e.query("variables CSS", None, 400, None);
        assert!(
            ids(&r).contains(&"d".to_string()),
            "the table must be returned: {:?}",
            ids(&r)
        );
    }

    #[test]
    fn a_path_asked_alone_reads_that_file_in_the_order_it_is_stored() {
        let e = manual();
        let ids = |q: &str| {
            e.query(q, None, 400, None)
                .records
                .iter()
                .map(|h| h.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("docs/diseno.md"), vec!["c", "d"]);
        assert_eq!(ids("`docs\\diseno.md`"), vec!["c", "d"]);
        assert_eq!(ids("./docs/negocio.md"), vec!["a", "b"]);
        assert_eq!(
            ids("diseño.md"),
            vec!["c", "d"],
            "a file name that only one source ends with"
        );
        let r = e.query("docs/diseno.md", None, 400, None);
        assert_eq!(r.confidence, Confidence::Strong);
        // The budget still applies, and the first section is always returned.
        assert_eq!(e.query("docs/diseno.md", None, 1, None).records.len(), 1);
        // A name that two sources share is not a path, and words around a path make it an ordinary query.
        let twins = Engine::new(
            DbMeta::default(),
            vec![
                section("x", "a/README.md", "A", "uno"),
                section("y", "b/README.md", "B", "dos"),
            ],
        );
        assert_eq!(twins.query("README.md", None, 400, None).records.len(), 2);
        assert!(twins.source_lookup("README.md", &[0, 1]).is_none());
        assert!(e
            .source_lookup(
                "qué dice docs/diseno.md sobre las variables",
                &[0, 1, 2, 3, 4]
            )
            .is_none());
    }

    #[test]
    fn the_headings_above_a_section_count_less_than_its_own() {
        let adr = |id: &str, leaf: &str| {
            section(
                id,
                "docs/adr-006.md",
                &format!("ADR-006: Aviso de alcohol y pagos > {leaf}"),
                "Texto de la decisión.",
            )
        };
        let e = Engine::new(
            DbMeta::default(),
            vec![
                adr("x1", "Contexto"),
                adr("x2", "Decisión"),
                adr("x3", "Consecuencias"),
                section(
                    "menu",
                    "docs/menu.md",
                    "La carta > Aviso de alcohol",
                    "Va junto a cócteles y cervezas.",
                ),
            ],
        );
        let r = e.query("aviso de alcohol", None, 400, None);
        assert_eq!(
            r.records[0].id,
            "menu",
            "{:?}",
            r.records.iter().map(|h| h.id.as_str()).collect::<Vec<_>>()
        );
    }
}
