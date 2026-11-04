//! `fds audit`: read or verify the guard's audit file.

use std::path::PathBuf;

use serde_json::json;

use crate::audit;
use crate::cli::AuditCmd;
use crate::print_json;

pub fn audit_cmd(cmd: &AuditCmd, json_out: bool) -> Result<u8, String> {
    let default = || PathBuf::from(audit::FILE);
    match cmd {
        AuditCmd::Show { file, limit } => {
            let path = file.clone().unwrap_or_else(default);
            let mut steps = audit::steps(&path)?;
            steps.reverse();
            if let Some(n) = limit {
                steps.truncate(*n);
            }
            if json_out {
                print_json(&serde_json::to_value(&steps).map_err(|e| e.to_string())?);
            } else if steps.is_empty() {
                println!("nothing recorded in {}", path.display());
            } else {
                for st in steps {
                    println!("{}  #{}  {}", st.at, st.order, st.text);
                }
            }
            Ok(0)
        }
        AuditCmd::Verify { file } => {
            let path = file.clone().unwrap_or_else(default);
            let r = audit::verify(&path)?;
            if json_out {
                print_json(
                    &json!({"entries": r.lines, "intact": r.broken.is_none(), "head": r.head, "broken_at": r.broken.as_ref().map(|b| b.0), "why": r.broken.as_ref().map(|b| b.1.clone())}),
                );
            } else if let Some((line, why)) = &r.broken {
                println!("BROKEN at line {line}: {why}");
            } else {
                println!(
                    "{} entries, the chain is intact. Last hash: {}",
                    r.lines, r.head
                );
                println!("Keep that hash where the agent cannot write: cut-off lines at the end only show against it.");
            }
            Ok(if r.broken.is_none() { 0 } else { 1 })
        }
    }
}
