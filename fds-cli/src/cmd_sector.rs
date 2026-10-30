//! `fds sector`: one database per agent, plus shared ones it may read.

use fds::v4::DbMeta;
use serde_json::{json, Value};

use crate::cli::SectorCmd;
use crate::text::today;
use crate::{db, ops, print_json, sectors};

pub fn sector_cmd(cmd: &SectorCmd, json_out: bool) -> Result<u8, String> {
    match cmd {
        SectorCmd::New {
            name,
            agent,
            reads,
            file,
            manifest,
            clearance,
        } => {
            let mut m = if manifest.exists() {
                sectors::Manifest::load(manifest)?
            } else {
                sectors::Manifest::new(manifest)
            };
            if m.get(name).is_some() {
                return Err(format!(
                    "sector `{name}` already exists in {}",
                    manifest.display()
                ));
            }
            let file = file.clone().unwrap_or_else(|| format!("{name}.fds"));
            let key = agent.then(sectors::new_key);
            m.sectors.push(sectors::SectorDef {
                name: name.clone(),
                file: file.clone(),
                reads: reads.clone(),
                key_sha256: key.as_deref().map(sectors::key_hash),
                clearance: clearance
                    .iter()
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect(),
            });
            let path = m.path_of(m.get(name).unwrap());
            let created = !path.exists();
            if created {
                let date = today();
                let mut meta = DbMeta {
                    name: name.clone(),
                    created: date.clone(),
                    updated: date,
                    ..Default::default()
                };
                // Checked before anything is written: a bad name or a
                // missing `reads` target leaves no stray database behind.
                m.save(manifest)?;
                db::save(&path, &mut meta, &[])?;
            } else {
                m.save(manifest)?;
            }
            if json_out {
                print_json(&json!({"name": name, "file": file, "created": created, "key": key}));
            } else {
                eprintln!(
                    "Sector `{name}` → {}{}",
                    path.display(),
                    if created {
                        " (new, empty)"
                    } else {
                        " (existing file)"
                    }
                );
                if let Some(k) = key {
                    eprintln!("Key for agent `{name}`, shown only now. Give it to the agent as FDS_KEY or as `Authorization: Bearer`:");
                    println!("{k}");
                }
            }
        }
        SectorCmd::Ls { manifest } => {
            let m = sectors::Manifest::load(manifest)?;
            let mut rows = Vec::new();
            for d in &m.sectors {
                let records = db::load(&m.path_of(d)).map(|(_, r)| r.len()).ok();
                rows.push(json!({"name": d.name, "file": d.file, "agent": d.key_sha256.is_some(), "reads": d.reads, "records": records}));
            }
            if json_out {
                print_json(&Value::Array(rows));
            } else {
                for r in &rows {
                    let reads: Vec<String> = r["reads"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect();
                    println!(
                        "{:<16} {:<6} {:>7} records  {:<20} reads: {}",
                        r["name"].as_str().unwrap_or(""),
                        if r["agent"].as_bool() == Some(true) {
                            "agent"
                        } else {
                            "shared"
                        },
                        r["records"]
                            .as_u64()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "?".into()),
                        r["file"].as_str().unwrap_or(""),
                        if reads.is_empty() {
                            "—".to_string()
                        } else {
                            reads.join(", ")
                        }
                    );
                }
            }
        }
        SectorCmd::Key { name, manifest } => {
            let mut m = sectors::Manifest::load(manifest)?;
            let key = sectors::new_key();
            let def = m
                .get_mut(name)
                .ok_or(format!("no sector `{name}` in {}", manifest.display()))?;
            def.key_sha256 = Some(sectors::key_hash(&key));
            m.save(manifest)?;
            if json_out {
                print_json(&json!({"name": name, "key": key}));
            } else {
                eprintln!(
                    "New key for agent `{name}`, shown only now; the old key no longer works:"
                );
                println!("{key}");
            }
        }
        SectorCmd::Promote {
            id,
            from,
            to,
            manifest,
        } => {
            let m = sectors::Manifest::load(manifest)?;
            let src = m.path_of(m.get(from).ok_or(format!("no sector `{from}`"))?);
            let dst = m.path_of(m.get(to).ok_or(format!("no sector `{to}`"))?);
            if src == dst {
                return Err("--from and --to are the same sector".into());
            }
            // Both locks, always in the same order: two promotions running
            // the opposite way can never hold one each and wait forever.
            let (first, second) = if src < dst {
                (&src, &dst)
            } else {
                (&dst, &src)
            };
            let _l1 = db::lock(first)?;
            let _l2 = db::lock(second)?;
            let (mut src_meta, mut src_records) = db::load(&src)?;
            let (mut dst_meta, mut dst_records) = db::load(&dst)?;
            let i = db::resolve_id(&src_records, id)?;
            let mut r = src_records[i].clone();
            let was = std::mem::replace(&mut r.verified_by, "human".into());
            r.last_verified = today();
            let new_id = ops::upsert(&mut dst_records, r);
            // The copy is saved before the original goes: a crash between
            // the two leaves the record twice, never nowhere.
            db::save(&dst, &mut dst_meta, &dst_records)?;
            src_records.remove(i);
            db::save(&src, &mut src_meta, &src_records)?;
            if json_out {
                print_json(&json!({"id": new_id, "from": from, "to": to, "was_verified_by": was}));
            } else {
                eprintln!("Promoted {} from `{from}` to `{to}`; it was {was}, it is now verified by a person.", &new_id[..new_id.len().min(12)]);
            }
        }
    }
    Ok(0)
}
