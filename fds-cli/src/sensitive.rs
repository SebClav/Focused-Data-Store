//! Sensitive records (SPEC.md §11.4): what an agent is not cleared to read is
//! replaced before anything is indexed, so it cannot be returned, quoted or
//! found.
//!
//! A record with `attrs.sensitivity` (`personal`, `secret`, anything the owner
//! names) is returned in full only to a caller whose sector lists that level in
//! its `clearance`. Everyone else gets a stand-in: the owner's own `derived`
//! text if there is one ("the customer lives in Lima"), else `[protected:
//! personal]`, with the record's id and title. The stand-in is what gets
//! indexed for them, not the original: a search over hidden words would answer
//! "does this record mention X?" without ever showing the record.

use fds::v4::Record;

use crate::text::word_count;

pub const ATTR: &str = "sensitivity";
pub const DERIVED: &str = "derived";

/// The record's sensitivity level, if it has one.
pub fn level(r: &Record) -> Option<&str> {
    r.attrs
        .get(ATTR)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
}

pub fn cleared(level: &str, clearance: &[String]) -> bool {
    clearance
        .iter()
        .any(|c| c.trim().eq_ignore_ascii_case(level))
}

/// True when `records` holds something this clearance may not read.
pub fn any_hidden(records: &[Record], clearance: &[String]) -> bool {
    records
        .iter()
        .any(|r| level(r).map_or(false, |l| !cleared(l, clearance)))
}

/// A copy of `records` as a caller with this `clearance` may see them.
pub fn redact(records: &[Record], clearance: &[String]) -> Vec<Record> {
    records
        .iter()
        .map(|r| match level(r) {
            Some(l) if !cleared(l, clearance) => {
                let derived = r
                    .attrs
                    .get(DERIVED)
                    .map(|d| d.trim().to_string())
                    .filter(|d| !d.is_empty());
                let body = derived.unwrap_or_else(|| format!("[protected: {l}]"));
                let mut attrs = std::collections::BTreeMap::new();
                attrs.insert(ATTR.to_string(), l.to_string());
                attrs.insert("protected".to_string(), "true".to_string());
                Record {
                    id: r.id.clone(),
                    kind: r.kind,
                    collection: r.collection.clone(),
                    question: r.question.clone(),
                    title: r.title.clone(),
                    tokens: word_count(&body),
                    body,
                    source: "[protected]".to_string(),
                    verified_by: r.verified_by.clone(),
                    last_verified: r.last_verified.clone(),
                    embedding: None,
                    rtype: r.rtype.clone(),
                    attrs,
                    links: Vec::new(),
                }
            }
            _ => r.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{Confidence, Engine, Scope};
    use fds::v4::{DbMeta, Kind};

    fn rec(question: &str, body: &str, level: Option<&str>, derived: Option<&str>) -> Record {
        let mut attrs = std::collections::BTreeMap::new();
        if let Some(l) = level {
            attrs.insert(ATTR.to_string(), l.to_string());
        }
        if let Some(d) = derived {
            attrs.insert(DERIVED.to_string(), d.to_string());
        }
        attrs.insert("dni".to_string(), "4455667".to_string());
        Record {
            id: crate::text::stable_id(question),
            kind: Kind::Fact,
            collection: "clientes".into(),
            question: Some(crate::text::canonical(question)),
            body: body.into(),
            title: question.into(),
            source: "clientes/juan.xlsx".into(),
            verified_by: "human".into(),
            last_verified: "2026-10-23".into(),
            tokens: 9,
            embedding: Some(vec![0.5, 0.5]),
            rtype: String::new(),
            attrs,
            links: vec!["otro".into()],
        }
    }

    fn records() -> Vec<Record> {
        vec![
            rec(
                "direccion de Juan Perez",
                "Juan vive en Calle Secreta 99, Miraflores",
                Some("personal"),
                Some("El cliente vive en Lima"),
            ),
            rec(
                "telefono de Marta Rojas",
                "Marta: 999 888 777",
                Some("personal"),
                None,
            ),
            rec("clave del servidor", "hunter2", Some("secret"), None),
            rec(
                "horario de atencion",
                "Lunes a sabado de 7 a 20",
                None,
                None,
            ),
        ]
    }

    #[test]
    fn a_record_without_clearance_becomes_its_stand_in() {
        let red = redact(&records(), &[]);
        let juan = &red[0];
        assert_eq!(
            juan.body, "El cliente vive en Lima",
            "the owner's own derived text"
        );
        let marta = &red[1];
        assert_eq!(marta.body, "[protected: personal]");
        assert_eq!(
            (marta.title.as_str(), marta.id.len()),
            ("telefono de Marta Rojas", 16),
            "id and title stay"
        );
        for r in &red[..3] {
            assert!(r.embedding.is_none() && r.links.is_empty() && r.source == "[protected]");
            assert!(
                !r.attrs.contains_key("dni") && !r.attrs.contains_key(DERIVED),
                "nothing else of the record survives: {:?}",
                r.attrs
            );
            assert_eq!(r.attrs.get("protected").map(String::as_str), Some("true"));
        }
        assert_eq!(red[3], records()[3], "an ordinary record is untouched");
    }

    /// conformance/policy.json, section `redact`.
    #[test]
    fn redaction_reproduces_the_conformance_vectors() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("conformance")
            .join("policy.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let red = &v["redact"];
        let records: Vec<Record> = red["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                let attrs = r["attrs"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(k, x)| (k.clone(), x.as_str().unwrap().to_string()))
                    .collect();
                let name = r["name"].as_str().unwrap();
                Record {
                    id: crate::text::stable_id(name),
                    kind: Kind::Fact,
                    collection: "c".into(),
                    question: None,
                    body: r["body"].as_str().unwrap().into(),
                    title: r["title"].as_str().unwrap().into(),
                    source: "vector".into(),
                    verified_by: "human".into(),
                    last_verified: "2026-10-23".into(),
                    tokens: 1,
                    embedding: Some(vec![1.0]),
                    rtype: String::new(),
                    attrs,
                    links: vec!["x".into()],
                }
            })
            .collect();
        for case in red["cases"].as_array().unwrap() {
            let clearance: Vec<String> = case["clearance"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_str().unwrap().to_string())
                .collect();
            let out = redact(&records, &clearance);
            for (name, want) in case["bodies"].as_object().unwrap() {
                let r = out
                    .iter()
                    .find(|r| r.id == crate::text::stable_id(name))
                    .unwrap();
                assert_eq!(
                    r.body,
                    want.as_str().unwrap(),
                    "{name} with clearance {clearance:?}"
                );
                if r.attrs.get("protected").is_some() {
                    assert_eq!(r.source, red["stand_in_source"].as_str().unwrap());
                    assert!(r.links.is_empty() && r.embedding.is_none());
                    assert_eq!(
                        r.attrs.keys().cloned().collect::<Vec<_>>(),
                        ["protected", "sensitivity"]
                    );
                }
            }
        }
    }

    #[test]
    fn clearance_is_per_level_and_ignores_case() {
        let personal = vec!["Personal".to_string()];
        let red = redact(&records(), &personal);
        assert!(
            red[0].body.contains("Calle Secreta"),
            "cleared for personal"
        );
        assert!(red[1].body.contains("999 888 777"));
        assert_eq!(red[2].body, "[protected: secret]", "not cleared for secret");
        assert!(!any_hidden(
            &records(),
            &["personal".to_string(), "secret".to_string()]
        ));
        assert!(any_hidden(&records(), &personal));
    }

    /// The search itself must not leak: asking for a word that exists only in
    /// the hidden body finds nothing, so nobody can probe what a record says.
    #[test]
    fn a_hidden_word_cannot_be_searched_for() {
        let scope = Scope {
            collection: None,
            verified_by: &[],
        };
        let full = Engine::new(DbMeta::default(), records());
        let hidden = Engine::new(DbMeta::default(), redact(&records(), &[]));
        let secret_word = "Secreta";
        assert!(
            full.query_scoped(secret_word, &scope, 400, None).confidence != Confidence::None,
            "the owner finds it"
        );
        let seen = hidden.query_scoped(secret_word, &scope, 400, None);
        assert_eq!(
            seen.confidence,
            Confidence::None,
            "an agent without clearance does not"
        );
        assert!(seen.records.is_empty());
        let hunter = hidden.query_scoped("hunter2", &scope, 400, None);
        assert!(hunter.records.iter().all(|h| !h.body.contains("hunter2")));
        // What it can ask is the title, and it gets the stand-in.
        let by_title = hidden.query_scoped("direccion de Juan Perez", &scope, 400, None);
        assert_eq!(by_title.records[0].body, "El cliente vive en Lima");
    }
}
