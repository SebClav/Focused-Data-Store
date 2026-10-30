//! Policies and the check (SPEC.md §11.1, §11.2).
//!
//! An agent can be persuaded; a comparison of numbers cannot. A policy is a
//! record a person verified, and `check` answers whether an action breaks one
//! with no model, no clock and no randomness: the same policies and the same
//! action always give the same answer. Anything it cannot evaluate is denied.

use fds::v4::Record;
use serde::{Deserialize, Serialize};

/// The `type` of a policy record.
pub const TYPE: &str = "policy";
/// Exit status of `fds check` when the action is denied.
pub const DENY_EXIT: u8 = 3;

/// What the guard does with a call that no policy maps to a check, and what a
/// check answers when no policy matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Fallback {
    #[default]
    Allow,
    Deny,
}

impl Fallback {
    pub fn parse(s: &str) -> Result<Fallback, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "allow" => Ok(Fallback::Allow),
            "deny" => Ok(Fallback::Deny),
            other => Err(format!("`{other}` is not a default; use allow or deny")),
        }
    }
}

/// How a tool call becomes a check (§11.3): `{name}` takes the call's argument of that name.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct GuardMap {
    pub tool: String,
    pub subject: String,
    pub action: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    pub id: String,
    pub subject: String,
    pub action: String,
    pub rule: String,
    pub min: Option<String>,
    pub max: Option<String>,
    pub values: Vec<String>,
    pub unit: String,
    pub text: String,
    pub verified_by: String,
    pub last_verified: String,
    pub guard: Option<GuardMap>,
}

/// The policies among `records` that count: type `policy` and verified by a
/// person. A policy a model wrote is ignored wherever it is stored, so a
/// compromised agent cannot loosen a rule by writing one.
pub fn policies(records: &[Record]) -> Vec<Policy> {
    let mut out: Vec<Policy> = records
        .iter()
        .filter(|r| r.rtype == TYPE && r.verified_by == "human")
        .filter_map(|r| {
            let a = |k: &str| {
                r.attrs
                    .get(k)
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            };
            Some(Policy {
                id: r.id.clone(),
                subject: a("subject")?,
                action: a("action").unwrap_or_else(|| "*".into()),
                rule: a("rule")?,
                min: a("min"),
                max: a("max"),
                values: a("values")
                    .map(|v| {
                        v.split(',')
                            .map(|x| x.trim().to_string())
                            .filter(|x| !x.is_empty())
                            .collect()
                    })
                    .unwrap_or_default(),
                unit: a("unit").unwrap_or_default(),
                text: if r.body.is_empty() {
                    r.title.clone()
                } else {
                    r.body.clone()
                },
                verified_by: r.verified_by.clone(),
                last_verified: r.last_verified.clone(),
                guard: a("guard").and_then(|g| serde_json::from_str(&g).ok()),
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn subject_matches(pattern: &str, subject: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => subject.starts_with(prefix),
        None => pattern == subject,
    }
}

fn action_matches(pattern: &str, action: &str) -> bool {
    pattern == "*" || pattern == action
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Because {
    pub id: String,
    pub rule: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub unit: String,
    pub text: String,
    pub verified_by: String,
    pub last_verified: String,
    /// Why this policy denied: the value outside its range, or what could not be evaluated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Decision {
    pub decision: &'static str,
    pub subject: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub because: Vec<Because>,
    /// No policy matched: the answer is the default, not a rule.
    pub default_applied: bool,
}

impl Decision {
    pub fn allowed(&self) -> bool {
        self.decision == "allow"
    }

    /// One line for a person or an audit log.
    pub fn summary(&self) -> String {
        let value = self
            .value
            .as_deref()
            .map(|v| format!(" = {v}"))
            .unwrap_or_default();
        let why = if self.default_applied {
            "no policy matches (the default)".to_string()
        } else {
            self.because
                .iter()
                .map(|b| format!("{} {}", b.rule, b.text))
                .collect::<Vec<_>>()
                .join("; ")
        };
        format!(
            "{}: {} {}{value} -- {why}",
            self.decision, self.action, self.subject
        )
    }
}

fn because(p: &Policy, reason: Option<String>) -> Because {
    Because {
        id: p.id.clone(),
        rule: p.rule.clone(),
        min: p.min.clone(),
        max: p.max.clone(),
        values: p.values.clone(),
        unit: p.unit.clone(),
        text: p.text.clone(),
        verified_by: p.verified_by.clone(),
        last_verified: p.last_verified.clone(),
        reason,
    }
}

/// A finite decimal number, or nothing: ASCII digits, an optional sign, an
/// optional fraction and exponent (`-18`, `+.5`, `1e3`). `inf`, `NaN`,
/// underscores and digits of other alphabets are not numbers (SPEC §11.2).
pub fn number(s: &str) -> Option<f64> {
    let t = s.trim();
    let b = t.strip_prefix(['+', '-']).unwrap_or(t);
    let (mantissa, exp) = match b.find(['e', 'E']) {
        Some(i) => (&b[..i], Some(&b[i + 1..])),
        None => (b, None),
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit());
    let mantissa_ok = match mantissa.split_once('.') {
        Some((i, f)) => {
            (digits(i) || i.is_empty())
                && (digits(f) || f.is_empty())
                && !(i.is_empty() && f.is_empty())
        }
        None => digits(mantissa),
    };
    let exp_ok = exp.map_or(true, |e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
    if !mantissa_ok || !exp_ok {
        return None;
    }
    t.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// `None` when the policy is satisfied; the reason when it is not. A policy
/// that cannot be evaluated is not satisfied.
fn violated(p: &Policy, value: Option<&str>) -> Option<String> {
    match p.rule.as_str() {
        "deny" => Some("denied outright".into()),
        "range" => {
            let (Some(lo), Some(hi)) = (
                p.min.as_deref().and_then(number),
                p.max.as_deref().and_then(number),
            ) else {
                return Some("the policy has no usable min and max".into());
            };
            let Some(v) = value else {
                return Some("no value was given to check against the range".into());
            };
            match number(v) {
                Some(n) if n >= lo && n <= hi => None,
                Some(_) => Some(format!("{v} is outside {lo}..{hi}")),
                None => Some(format!("`{v}` is not a number")),
            }
        }
        "one_of" => {
            if p.values.is_empty() {
                return Some("the policy lists no values".into());
            }
            match value {
                Some(v) if p.values.iter().any(|x| x == v.trim()) => None,
                Some(v) => Some(format!("`{v}` is not one of the allowed values")),
                None => Some("no value was given to check against the list".into()),
            }
        }
        other => Some(format!("unknown rule `{other}`")),
    }
}

/// Every policy whose subject and action match is evaluated; one that is
/// violated is enough to deny. With no match the answer is `default`.
pub fn check(
    policies: &[Policy],
    subject: &str,
    action: &str,
    value: Option<&str>,
    default: Fallback,
) -> Decision {
    let matching: Vec<&Policy> = policies
        .iter()
        .filter(|p| subject_matches(&p.subject, subject) && action_matches(&p.action, action))
        .collect();
    let mut denying = Vec::new();
    let mut passing = Vec::new();
    for p in &matching {
        match violated(p, value) {
            Some(reason) => denying.push(because(p, Some(reason))),
            None => passing.push(because(p, None)),
        }
    }
    let base = |decision: &'static str, because: Vec<Because>, default_applied: bool| Decision {
        decision,
        subject: subject.to_string(),
        action: action.to_string(),
        value: value.map(String::from),
        because,
        default_applied,
    };
    if !denying.is_empty() {
        return base("deny", denying, false);
    }
    if matching.is_empty() {
        return base(
            if default == Fallback::Deny {
                "deny"
            } else {
                "allow"
            },
            Vec::new(),
            true,
        );
    }
    base("allow", passing, false)
}

/// Fills `{name}` in a guard template from a tool call's arguments. A name the
/// call does not have makes the whole template `None`: it cannot be checked.
pub fn fill(template: &str, args: &serde_json::Value) -> Option<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}')? + open;
        let name = &rest[open + 1..close];
        let v = args.get(name)?;
        match v {
            serde_json::Value::String(s) => out.push_str(s),
            serde_json::Value::Number(n) => out.push_str(&n.to_string()),
            serde_json::Value::Bool(b) => out.push_str(&b.to_string()),
            _ => return None,
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn policy(
        subject: &str,
        action: &str,
        rule: &str,
        extra: &[(&str, &str)],
        text: &str,
    ) -> Record {
        let mut attrs: BTreeMap<String, String> =
            [("subject", subject), ("action", action), ("rule", rule)]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        for (k, v) in extra {
            attrs.insert(k.to_string(), v.to_string());
        }
        Record {
            id: crate::text::stable_id(&format!("{subject}{action}{rule}{text}")),
            kind: fds::v4::Kind::Fact,
            collection: "policy".into(),
            question: Some(text.to_lowercase()),
            body: text.into(),
            title: text.into(),
            source: "owner".into(),
            verified_by: "human".into(),
            last_verified: "2026-10-23".into(),
            tokens: 5,
            embedding: None,
            rtype: TYPE.into(),
            attrs,
            links: Vec::new(),
        }
    }

    fn fridge() -> Vec<Policy> {
        policies(&[
            policy(
                "fridge.chamber3.setpoint",
                "set",
                "range",
                &[("min", "-20"), ("max", "-16"), ("unit", "\u{b0}C")],
                "Below -20 \u{b0}C the stock is damaged.",
            ),
            policy("db.prod.*", "drop", "deny", &[], "Nobody drops production."),
            policy(
                "backup.target",
                "write",
                "one_of",
                &[("values", "copy,staging")],
                "Backups go to a copy.",
            ),
        ])
    }

    #[test]
    fn a_range_allows_inside_and_denies_outside_and_denies_what_is_not_a_number() {
        let p = fridge();
        let go = |v: &str| {
            check(
                &p,
                "fridge.chamber3.setpoint",
                "set",
                Some(v),
                Fallback::Allow,
            )
        };
        assert_eq!(go("-18").decision, "allow");
        assert_eq!(go("-20").decision, "allow", "bounds are inclusive");
        assert_eq!(go("-16").decision, "allow");
        let d = go("-40");
        assert_eq!(d.decision, "deny");
        assert_eq!(
            (
                d.because[0].min.as_deref(),
                d.because[0].max.as_deref(),
                d.because[0].unit.as_str()
            ),
            (Some("-20"), Some("-16"), "\u{b0}C")
        );
        assert!(d.because[0].text.contains("stock is damaged"));
        for bad in ["warm", "", "NaN", "inf", "1e999"] {
            assert_eq!(go(bad).decision, "deny", "`{bad}` must not pass a range");
        }
        assert_eq!(
            check(&p, "fridge.chamber3.setpoint", "set", None, Fallback::Allow).decision,
            "deny",
            "a range with no value cannot be met"
        );
    }

    #[test]
    fn deny_and_one_of_and_prefixes_and_wildcards() {
        let p = fridge();
        assert_eq!(
            check(&p, "db.prod.customers", "drop", None, Fallback::Allow).decision,
            "deny",
            "a trailing * matches a prefix"
        );
        assert_eq!(
            check(&p, "db.prod", "drop", None, Fallback::Allow).decision,
            "allow",
            "...and only a prefix: `db.prod` is not `db.prod.`"
        );
        assert_eq!(
            check(&p, "db.prod.customers", "read", None, Fallback::Allow).decision,
            "allow",
            "another action is not covered"
        );
        assert_eq!(
            check(&p, "backup.target", "write", Some("copy"), Fallback::Allow).decision,
            "allow"
        );
        assert_eq!(
            check(&p, "backup.target", "write", Some("prod"), Fallback::Allow).decision,
            "deny"
        );
        let any = policies(&[policy("vault.*", "*", "deny", &[], "Closed.")]);
        assert_eq!(
            check(&any, "vault.keys", "read", None, Fallback::Allow).decision,
            "deny",
            "action * matches every action"
        );
    }

    #[test]
    fn deny_overrides_allow_and_the_default_decides_when_nothing_matches() {
        let both = policies(&[
            policy("x.y", "set", "range", &[("min", "0"), ("max", "10")], "A"),
            policy("x.*", "set", "range", &[("min", "5"), ("max", "6")], "B"),
        ]);
        assert_eq!(
            check(&both, "x.y", "set", Some("3"), Fallback::Allow).decision,
            "deny",
            "inside A, outside B: one denial is enough"
        );
        assert_eq!(
            check(&both, "x.y", "set", Some("5.5"), Fallback::Allow).decision,
            "allow"
        );
        let none = check(&both, "other", "set", Some("1"), Fallback::Allow);
        assert!(none.allowed() && none.default_applied && none.because.is_empty());
        let closed = check(&both, "other", "set", Some("1"), Fallback::Deny);
        assert!(!closed.allowed() && closed.default_applied);
    }

    #[test]
    fn what_cannot_be_evaluated_is_denied() {
        let broken = policies(&[
            policy("a", "set", "range", &[("min", "x")], "bad bounds"),
            policy("b", "set", "mystery", &[], "unknown rule"),
            policy("c", "set", "one_of", &[], "no values"),
        ]);
        for subject in ["a", "b", "c"] {
            let d = check(&broken, subject, "set", Some("1"), Fallback::Allow);
            assert_eq!(d.decision, "deny", "{subject}");
            assert!(d.because[0].reason.is_some(), "{subject}: {d:?}");
        }
    }

    #[test]
    fn only_a_policy_a_person_verified_counts() {
        let mut written_by_an_agent = policy(
            "fridge.chamber3.setpoint",
            "set",
            "range",
            &[("min", "-80"), ("max", "40")],
            "Anything goes.",
        );
        written_by_an_agent.verified_by = "model:ops".into();
        let mut p = fridge();
        p.extend(policies(&[written_by_an_agent]));
        assert_eq!(
            check(
                &p,
                "fridge.chamber3.setpoint",
                "set",
                Some("-40"),
                Fallback::Allow
            )
            .decision,
            "deny",
            "the agent's policy is not even read"
        );
        assert_eq!(p.len(), 3);
    }

    #[test]
    fn the_same_question_always_gets_the_same_answer() {
        let p = fridge();
        let first = serde_json::to_string(&check(
            &p,
            "fridge.chamber3.setpoint",
            "set",
            Some("-40"),
            Fallback::Allow,
        ))
        .unwrap();
        for _ in 0..50 {
            assert_eq!(
                serde_json::to_string(&check(
                    &p,
                    "fridge.chamber3.setpoint",
                    "set",
                    Some("-40"),
                    Fallback::Allow
                ))
                .unwrap(),
                first
            );
        }
        assert!(first.contains(r#""decision":"deny""#) && first.contains(r#""because""#));
    }

    fn vectors() -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("conformance")
            .join("policy.json");
        serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        )
        .unwrap()
    }

    /// conformance/policy.json: the cases every implementation must reproduce.
    #[test]
    fn the_check_reproduces_the_conformance_vectors() {
        let v = vectors();
        let records: Vec<Record> = v["policies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let mut attrs = BTreeMap::new();
                for k in ["subject", "action", "rule", "min", "max", "values", "unit"] {
                    if let Some(x) = p[k].as_str() {
                        attrs.insert(k.to_string(), x.to_string());
                    }
                }
                let name = p["name"].as_str().unwrap();
                Record {
                    id: crate::text::stable_id(name),
                    kind: fds::v4::Kind::Fact,
                    collection: "policy".into(),
                    question: Some(name.to_string()),
                    body: name.to_string(),
                    title: name.to_string(),
                    source: "vector".into(),
                    verified_by: p["verified_by"].as_str().unwrap_or("human").to_string(),
                    last_verified: "2026-10-23".into(),
                    tokens: 1,
                    embedding: None,
                    rtype: TYPE.into(),
                    attrs,
                    links: Vec::new(),
                }
            })
            .collect();
        let all = policies(&records);
        for c in v["checks"].as_array().unwrap() {
            let default = c["default"]
                .as_str()
                .map(|d| Fallback::parse(d).unwrap())
                .unwrap_or_default();
            let d = check(
                &all,
                c["subject"].as_str().unwrap(),
                c["action"].as_str().unwrap(),
                c["value"].as_str(),
                default,
            );
            let mut because: Vec<String> = d.because.iter().map(|b| b.text.clone()).collect();
            because.sort();
            let mut want: Vec<String> = c["because"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_str().unwrap().to_string())
                .collect();
            want.sort();
            let label = format!("{} {} = {:?}", c["action"], c["subject"], c["value"]);
            assert_eq!(d.decision, c["decision"].as_str().unwrap(), "{label}");
            assert_eq!(
                d.default_applied,
                c["default_applied"].as_bool().unwrap(),
                "{label}"
            );
            assert_eq!(because, want, "{label}");
        }
        for c in v["fill"].as_array().unwrap() {
            let got = fill(c["template"].as_str().unwrap(), &c["args"]);
            assert_eq!(
                got.as_deref(),
                c["expected"].as_str(),
                "fill {} {}",
                c["template"],
                c["args"]
            );
        }
    }

    #[test]
    fn a_guard_template_fills_from_the_call_and_refuses_what_is_missing() {
        let args = json!({"chamber": "chamber3", "celsius": -40, "dry_run": false, "name": "x"});
        assert_eq!(
            fill("fridge.{chamber}.setpoint", &args).as_deref(),
            Some("fridge.chamber3.setpoint")
        );
        assert_eq!(fill("{celsius}", &args).as_deref(), Some("-40"));
        assert_eq!(fill("{dry_run}", &args).as_deref(), Some("false"));
        assert_eq!(fill("plain", &args).as_deref(), Some("plain"));
        assert_eq!(
            fill("a.{missing}", &args),
            None,
            "a call without the argument cannot be mapped"
        );
        assert_eq!(
            fill("{chamber", &args),
            None,
            "an unclosed brace is refused"
        );
        assert_eq!(
            fill("{nested}", &json!({"nested": {"a": 1}})),
            None,
            "objects are not values"
        );
    }
}
