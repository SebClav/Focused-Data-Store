//! Two-stage FDS retrieval: precise compiled answers first, lexical retrieval
//! second. The returned shape stays stable for CLI and future API callers.

use std::collections::HashSet;

use fds::v4::{Kind, Record};
use serde_json::{json, Value};

use crate::output::public_record;
use crate::text::{canonical, search_terms, token_key};

fn no_match() -> Value {
    json!({"confidence": "none", "results": []})
}

pub fn query_records(
    records: &[Record],
    query: &str,
    collection: Option<&str>,
    budget: u32,
    include_links: bool,
    verified_by: Option<&str>,
) -> Value {
    let visible = |r: &&Record| {
        r.rtype != "context"
            && collection.is_none_or(|c| c == r.collection)
            && verified_by.is_none_or(|list| {
                list.split(',')
                    .map(str::trim)
                    .any(|w| match w.strip_suffix(':') {
                        Some(prefix) => r.verified_by.starts_with(prefix),
                        None => r.verified_by == w,
                    })
            })
    };

    // Prefer an exact normalized fact/procedure question before fuzzy retrieval.
    // The same question can be answered in several collections: return every
    // answer rather than picking one.
    let qcanon = canonical(query);
    let qkey = token_key(&qcanon);
    let compiled: Vec<Value> = records
        .iter()
        .filter(visible)
        .filter(|r| matches!(r.kind, Kind::Fact | Kind::Procedure))
        .filter(|r| {
            r.question
                .as_deref()
                .is_some_and(|stored| canonical(stored) == qcanon || token_key(stored) == qkey)
        })
        .map(|r| {
            let mut result = public_record(r);
            result["confidence"] = json!("exact");
            result["match"] = json!("compiled");
            result
        })
        .collect();
    if !compiled.is_empty() {
        return json!({"confidence": "exact", "results": compiled});
    }

    // Fall back to a small BM25-style lexical ranking when there is no exact hit.
    let eligible: Vec<&Record> = records.iter().filter(visible).collect();
    let indexed: Vec<Vec<String>> = eligible.iter().map(|r| indexed_terms(r)).collect();
    let terms = search_terms(query);
    if terms.is_empty() {
        return no_match();
    }
    let mut scored = bm25(&indexed, &terms);
    if scored.is_empty() {
        return no_match();
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(5);

    let first = eligible[scored[0].0];
    let first_terms = search_terms(&format!(
        "{} {} {} {}",
        first.title,
        first.body,
        first.source,
        first.question.as_deref().unwrap_or("")
    ));
    // These labels describe term coverage, not factual certainty or verification.
    let confidence = if terms.iter().all(|t| first_terms.contains(t)) {
        "strong"
    } else {
        "weak"
    };

    let mut result_ids = HashSet::new();
    let mut result_rows = Vec::new();
    let mut used = 0u32;
    for (i, _) in scored {
        let r = eligible[i];
        if result_ids.insert(r.id.as_str()) && (result_rows.is_empty() || used + r.tokens <= budget)
        {
            used = used.saturating_add(r.tokens);
            let mut row = public_record(r);
            row["match"] = json!("keyword");
            result_rows.push(row);
        }
    }

    if include_links {
        let base: Vec<&Record> = records.iter().filter(visible).collect();
        let roots: Vec<&Record> = result_rows
            .iter()
            .filter_map(|row| records.iter().find(|r| row["id"] == r.id))
            .collect();
        for root in roots {
            for target in &root.links {
                let target_c = canonical(target);
                let linked = base.iter().find(|r| {
                    r.question
                        .as_deref()
                        .is_some_and(|q| canonical(q) == target_c)
                        || canonical(&r.title) == target_c
                });
                if let Some(r) = linked {
                    if result_ids.insert(r.id.as_str()) && used + r.tokens <= budget {
                        used += r.tokens;
                        let mut row = public_record(r);
                        row["match"] = json!("link");
                        result_rows.push(row);
                    }
                }
            }
        }
    }
    json!({"confidence": confidence, "results": result_rows})
}

/// Search terms of a record, with title and question weighted three times.
fn indexed_terms(r: &Record) -> Vec<String> {
    let mut text = format!("{} {} {}", r.title, r.title, r.title);
    if let Some(q) = &r.question {
        text.push_str(&format!(" {q} {q} {q}"));
    }
    text.push_str(&format!(" {} {}", r.body, r.source));
    search_terms(&text)
}

/// BM25 score of every document that contains at least one query term.
fn bm25(indexed: &[Vec<String>], terms: &[String]) -> Vec<(usize, f64)> {
    let n = indexed.len() as f64;
    let avg_len = indexed.iter().map(Vec::len).sum::<usize>() as f64 / n.max(1.0);
    let mut scored = Vec::new();
    for (i, doc) in indexed.iter().enumerate() {
        let dl = doc.len() as f64;
        let mut score = 0.0;
        for term in terms {
            let tf = doc.iter().filter(|x| *x == term).count() as f64;
            if tf == 0.0 {
                continue;
            }
            let df = indexed
                .iter()
                .filter(|d| d.iter().any(|x| x == term))
                .count() as f64;
            let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            score += idf * (tf * 2.5) / (tf + 1.5 * (1.0 - 0.75 + 0.75 * dl / avg_len.max(1.0)));
        }
        if score > 0.0 {
            scored.push((i, score));
        }
    }
    scored
}
