//! The guard (SPEC.md §11.3): a proxy between an agent and the real tool
//! server, speaking MCP over stdio on both sides.
//!
//! The agent talks to `fds guard`; `fds guard` starts the real server as a
//! child, keeps its credentials (its environment) to itself, and forwards
//! everything the agent sends -- except `tools/call`, which it first maps to a
//! check (policy.rs). A denied call never reaches the tool: the agent gets a
//! tool error with the rule and its text. Every `tools/call` decision is
//! appended to the audit file (audit.rs). No model is involved anywhere.
//!
//! What the guard sends on is the message it parsed, written out again, never
//! the line it read: two JSON parsers can disagree about a line (a key given
//! twice, say), and the tool must act on exactly what was checked.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::policy::{self, Decision, Fallback, Policy};
use crate::sectors::Manifest;
use crate::text::now;
use crate::{audit, db, mcp};

pub struct Config {
    pub manifest: PathBuf,
    pub agent: String,
    pub audit: PathBuf,
    pub command: Vec<String>,
}

/// The policies and the default, re-read when any file they come from changes,
/// so a rule a person just changed applies to the next call.
pub struct Rules {
    manifest: PathBuf,
    stamps: Vec<Option<db::Stamp>>,
    pub policies: Vec<Policy>,
    pub fallback: Fallback,
}

impl Rules {
    pub fn load(manifest: &Path) -> Result<Rules, String> {
        let m = Manifest::load(manifest)?;
        let mut policies = Vec::new();
        let mut stamps = vec![db::stamp(manifest)];
        for def in &m.sectors {
            let path = m.path_of(def);
            stamps.push(db::stamp(&path));
            policies.extend(policy::policies(&db::load(&path)?.1));
        }
        let fallback = m.default.as_deref().map(Fallback::parse).transpose()?.unwrap_or_default();
        Ok(Rules { manifest: manifest.to_path_buf(), stamps, policies, fallback })
    }

    /// Reloads if a file changed. A change that cannot be read keeps the last
    /// good rules and says so; the guard never falls back to "no rules".
    pub fn refresh(&mut self) {
        let m = match Manifest::load(&self.manifest) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("fds guard: {e}; still using the rules as they were");
                return;
            }
        };
        let mut now = vec![db::stamp(&self.manifest)];
        now.extend(m.sectors.iter().map(|d| db::stamp(&m.path_of(d))));
        if now != self.stamps {
            match Rules::load(&self.manifest) {
                Ok(fresh) => *self = fresh,
                Err(e) => eprintln!("fds guard: {e}; still using the rules as they were"),
            }
        }
    }
}

pub struct Verdict {
    pub allow: bool,
    /// For the agent and the audit: why, in the owner's words where there are any.
    pub reason: String,
    pub checks: Vec<Decision>,
}

/// Maps one tool call to checks and runs them. Denied if any check denies, if
/// the call lacks an argument a policy needs, or if no policy maps the tool and
/// the default is deny.
pub fn decide(rules: &Rules, tool: &str, args: &Value) -> Verdict {
    let maps: Vec<&policy::GuardMap> = rules.policies.iter().filter_map(|p| p.guard.as_ref()).filter(|g| g.tool == tool).collect();
    if maps.is_empty() {
        return match rules.fallback {
            Fallback::Allow => Verdict { allow: true, reason: "no policy governs this tool".into(), checks: Vec::new() },
            Fallback::Deny => Verdict { allow: false, reason: format!("no policy covers the tool `{tool}`, and the default is deny"), checks: Vec::new() },
        };
    }
    let mut seen = HashSet::new();
    let mut checks = Vec::new();
    for g in maps {
        let subject = policy::fill(&g.subject, args);
        let action = policy::fill(&g.action, args);
        let value = g.value.as_deref().map(|t| policy::fill(t, args));
        let (Some(subject), Some(action)) = (subject, action) else {
            return Verdict { allow: false, reason: format!("the call to `{tool}` lacks an argument a policy needs to check it"), checks };
        };
        let value = match value {
            None => None,
            Some(Some(v)) => Some(v),
            Some(None) => {
                return Verdict { allow: false, reason: format!("the call to `{tool}` lacks the value a policy needs to check it"), checks };
            }
        };
        if seen.insert((subject.clone(), action.clone(), value.clone())) {
            checks.push(policy::check(&rules.policies, &subject, &action, value.as_deref(), rules.fallback));
        }
    }
    let denials: Vec<String> = checks
        .iter()
        .filter(|c| !c.allowed())
        .flat_map(|c| {
            if c.default_applied {
                vec![format!("no policy covers {} {}, and the default is deny", c.action, c.subject)]
            } else {
                c.because.iter().map(|b| format!("{} ({})", b.text, b.reason.clone().unwrap_or_else(|| b.rule.clone()))).collect()
            }
        })
        .collect();
    if denials.is_empty() {
        Verdict { allow: true, reason: "allowed by policy".into(), checks }
    } else {
        Verdict { allow: false, reason: denials.join("; "), checks }
    }
}

fn audit_fields(agent: &str, tool: &str, args: &Value, v: &Verdict) -> Map<String, Value> {
    let checks: Vec<Value> = v
        .checks
        .iter()
        .map(|c| {
            json!({
                "subject": c.subject, "action": c.action, "value": c.value, "decision": c.decision,
                "rules": c.because.iter().map(|b| b.id.clone()).collect::<Vec<_>>(),
                "default_applied": c.default_applied,
            })
        })
        .collect();
    json!({
        "at": now(), "agent": agent, "tool": tool, "args": audit::shorten(args),
        "decision": if v.allow { "allow" } else { "deny" }, "reason": v.reason, "checks": checks,
    })
    .as_object()
    .cloned()
    .unwrap_or_default()
}

fn refusal(req: &Value, text: String) -> Value {
    let mut result = json!({"content": [{"type": "text", "text": text}], "isError": true});
    if mcp::requested_version(req).is_some() {
        result["resultType"] = json!("complete");
    }
    json!({"jsonrpc": "2.0", "id": req.get("id").cloned().unwrap_or(Value::Null), "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// What to do with one line from the agent.
pub enum Action {
    /// Write this to the real server.
    Forward(String),
    /// Answer the agent with this; the server never sees the line.
    Reply(Value),
    /// Nothing to send anywhere.
    Drop,
}

/// The whole decision for one line, without any I/O but the audit file.
pub fn handle(rules: &mut Rules, cfg_agent: &str, audit_path: &Path, line: &str) -> Action {
    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Action::Reply(rpc_error(Value::Null, -32700, "the guard could not parse this message, so it did not pass it on")),
    };
    // A batch could hide a tools/call from the check: refuse it whole.
    if req.is_array() {
        return Action::Reply(rpc_error(Value::Null, -32600, "the guard does not accept batches; send one request at a time"));
    }
    if req.get("method").and_then(|m| m.as_str()) != Some("tools/call") {
        return Action::Forward(serde_json::to_string(&req).unwrap_or_default());
    }
    rules.refresh();
    let tool = req.pointer("/params/name").and_then(|n| n.as_str()).unwrap_or("").to_string();
    let args = req.pointer("/params/arguments").cloned().unwrap_or_else(|| json!({}));
    let verdict = decide(rules, &tool, &args);
    if let Err(e) = audit::append(audit_path, audit_fields(cfg_agent, &tool, &args, &verdict)) {
        // A decision that cannot be recorded is not taken: the box must not go dark.
        eprintln!("fds guard: cannot write the audit: {e}");
        return Action::Reply(refusal(&req, format!("FDS guard: the audit file cannot be written ({e}), so this call was not made.")));
    }
    if verdict.allow {
        Action::Forward(serde_json::to_string(&req).unwrap_or_default())
    } else if req.get("id").is_some() {
        Action::Reply(refusal(
            &req,
            format!(
                "FDS guard: denied. {} This was decided by a rule a person verified, not by a model: do not retry with other values or another tool to get around it. A person can change the rule.",
                verdict.reason
            ),
        ))
    } else {
        Action::Drop
    }
}

/// Runs the guard until the agent or the server closes its side. Returns the
/// server's exit code.
pub async fn run(cfg: Config) -> Result<u8, String> {
    let mut rules = Rules::load(&cfg.manifest)?;
    let (program, rest) = cfg.command.split_first().ok_or("fds guard needs the real server's command after `--`")?;
    let mut child = Command::new(program)
        .args(rest)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot start `{program}`: {e}"))?;
    let mut to_child = child.stdin.take().ok_or("cannot reach the server's input")?;
    let from_child = child.stdout.take().ok_or("cannot reach the server's output")?;
    let out = Arc::new(Mutex::new(tokio::io::stdout()));

    let agent_side = {
        let out = out.clone();
        let (agent, audit_path) = (cfg.agent.clone(), cfg.audit.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match handle(&mut rules, &agent, &audit_path, &line) {
                    Action::Forward(msg) => {
                        if to_child.write_all(format!("{msg}\n").as_bytes()).await.is_err() || to_child.flush().await.is_err() {
                            break;
                        }
                    }
                    Action::Reply(v) => {
                        let mut o = out.lock().await;
                        let _ = o.write_all(format!("{v}\n").as_bytes()).await;
                        let _ = o.flush().await;
                    }
                    Action::Drop => {}
                }
            }
            // The agent is gone: closing the server's input lets it finish.
            drop(to_child);
        })
    };
    let server_side = {
        let out = out.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(from_child).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut o = out.lock().await;
                let _ = o.write_all(format!("{line}\n").as_bytes()).await;
                let _ = o.flush().await;
            }
        })
    };

    let status = tokio::select! {
        s = child.wait() => { let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server_side).await; s }
        _ = agent_side => {
            match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
                Ok(s) => s,
                Err(_) => { let _ = child.kill().await; child.wait().await }
            }
        }
    };
    Ok(status.ok().and_then(|s| s.code()).unwrap_or(1).clamp(0, 255) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fds::v4::{Kind, Record};
    use std::collections::BTreeMap;

    fn policy_record(subject: &str, action: &str, rule: &str, extra: &[(&str, &str)], guard: Option<Value>, text: &str) -> Record {
        let mut attrs: BTreeMap<String, String> = [("subject", subject), ("action", action), ("rule", rule)].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        for (k, v) in extra {
            attrs.insert(k.to_string(), v.to_string());
        }
        if let Some(g) = guard {
            attrs.insert("guard".into(), g.to_string());
        }
        Record {
            id: crate::text::stable_id(&format!("{subject}{action}{rule}{text}")),
            kind: Kind::Fact,
            collection: "policy".into(),
            question: Some(text.to_lowercase()),
            body: text.into(),
            title: text.into(),
            source: "owner".into(),
            verified_by: "human".into(),
            last_verified: "2026-10-23".into(),
            tokens: 4,
            embedding: None,
            rtype: policy::TYPE.into(),
            attrs,
            links: Vec::new(),
        }
    }

    /// A manifest on disk with one sector holding the fridge policies, because
    /// the guard reads what a person put in the files.
    fn rules(default: Option<&str>, name: &str) -> (Rules, PathBuf) {
        let dir = std::env::temp_dir().join(format!("fds-guard-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let records = vec![
            policy_record(
                "fridge.chamber3.setpoint", "set", "range", &[("min", "-20"), ("max", "-16"), ("unit", "\u{b0}C")],
                Some(json!({"tool": "set_temperature", "subject": "fridge.{chamber}.setpoint", "action": "set", "value": "{celsius}"})),
                "Below -20 \u{b0}C the stock is damaged.",
            ),
            policy_record("db.prod.*", "drop", "deny", &[], Some(json!({"tool": "run_sql_drop", "subject": "db.prod.{table}", "action": "drop"})), "Nobody drops production."),
        ];
        db::save(&dir.join("policies.fds"), &mut fds::v4::DbMeta::default(), &records).unwrap();
        let mut manifest = json!({"sectors": [{"name": "policies", "file": "policies.fds"}]});
        if let Some(d) = default {
            manifest["default"] = json!(d);
        }
        std::fs::write(dir.join("sectors.json"), manifest.to_string()).unwrap();
        (Rules::load(&dir.join("sectors.json")).unwrap(), dir)
    }

    #[test]
    fn a_call_is_mapped_checked_and_denied_or_allowed() {
        let (r, dir) = rules(None, "decide");
        let ok = decide(&r, "set_temperature", &json!({"chamber": "chamber3", "celsius": -18}));
        assert!(ok.allow, "{}", ok.reason);
        let bad = decide(&r, "set_temperature", &json!({"chamber": "chamber3", "celsius": -40}));
        assert!(!bad.allow && bad.reason.contains("stock is damaged"), "{}", bad.reason);
        assert!(!decide(&r, "run_sql_drop", &json!({"table": "customers"})).allow);
        // Another chamber has no policy: the default (allow) decides.
        assert!(decide(&r, "set_temperature", &json!({"chamber": "chamber9", "celsius": 100})).allow);
        // A call that lacks what the policy needs cannot be checked: denied.
        let missing = decide(&r, "set_temperature", &json!({"chamber": "chamber3"}));
        assert!(!missing.allow && missing.reason.contains("lacks"), "{}", missing.reason);
        assert!(decide(&r, "unrelated_tool", &json!({})).allow, "no policy governs it, the default allows");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn default_deny_closes_what_no_policy_covers() {
        let (r, dir) = rules(Some("deny"), "closed");
        assert!(!decide(&r, "unrelated_tool", &json!({})).allow);
        assert!(!decide(&r, "set_temperature", &json!({"chamber": "chamber9", "celsius": -18})).allow, "no policy matches chamber9, and the default is deny");
        assert!(decide(&r, "set_temperature", &json!({"chamber": "chamber3", "celsius": -18})).allow);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn handle_forwards_what_passes_answers_what_is_denied_and_audits_both() {
        let (mut r, dir) = rules(None, "handle");
        let audit_path = dir.join(audit::FILE);
        let call = |id: i64, celsius: i64| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "set_temperature", "arguments": {"chamber": "chamber3", "celsius": celsius}}}).to_string();

        match handle(&mut r, "agent-1", &audit_path, &call(1, -18)) {
            Action::Forward(m) => assert!(m.contains("set_temperature")),
            _ => panic!("an allowed call must be forwarded"),
        }
        match handle(&mut r, "agent-1", &audit_path, &call(2, -40)) {
            Action::Reply(v) => {
                assert_eq!(v["id"], 2);
                assert_eq!(v["result"]["isError"], true);
                assert!(v["result"]["content"][0]["text"].as_str().unwrap().contains("stock is damaged"));
            }
            _ => panic!("a denied call must be answered, not forwarded"),
        }
        // Anything that is not a tool call goes through untouched.
        assert!(matches!(handle(&mut r, "a", &audit_path, r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#), Action::Forward(_)));
        // Garbage and batches are never passed on.
        assert!(matches!(handle(&mut r, "a", &audit_path, "{not json"), Action::Reply(_)));
        let batch = format!("[{}]", call(4, -40));
        assert!(matches!(handle(&mut r, "a", &audit_path, &batch), Action::Reply(_)));

        let rep = audit::verify(&audit_path).unwrap();
        assert_eq!((rep.lines, rep.broken.is_none()), (2, true), "both tool calls are on the record");
        let text = std::fs::read_to_string(&audit_path).unwrap();
        assert!(text.contains("\"decision\":\"deny\"") && text.contains("\"decision\":\"allow\"") && text.contains("agent-1"));
        std::fs::remove_dir_all(dir).ok();
    }

    /// The tool must act on what was checked: a key given twice must not let the
    /// agent show the guard one value and the server another.
    #[test]
    fn the_server_gets_the_message_that_was_checked_not_the_line_that_was_read() {
        let (mut r, dir) = rules(None, "dup");
        let audit_path = dir.join(audit::FILE);
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"set_temperature","arguments":{"chamber":"chamber3","celsius":-40,"celsius":-18}}}"#;
        // serde keeps the last duplicate (-18), the guard checks -18...
        match handle(&mut r, "a", &audit_path, line) {
            Action::Forward(m) => {
                // ...and what is forwarded has exactly one `celsius`, the one that was checked.
                assert_eq!(m.matches("celsius").count(), 1, "{m}");
                assert!(m.contains("-18") && !m.contains("-40"), "{m}");
            }
            _ => panic!("expected forward"),
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_call_is_refused_if_it_cannot_be_recorded() {
        let (mut r, dir) = rules(None, "noaudit");
        // The audit path is a directory: the append fails.
        let bad = dir.join("a-folder");
        std::fs::create_dir_all(&bad).unwrap();
        let line = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "set_temperature", "arguments": {"chamber": "chamber3", "celsius": -18}}}).to_string();
        match handle(&mut r, "a", &bad, &line) {
            Action::Reply(v) => assert!(v["result"]["content"][0]["text"].as_str().unwrap().contains("audit")),
            _ => panic!("an unrecorded decision must not be taken"),
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
