//! Command-line interface for the FDS local knowledge database.
//!
//! The CLI reads and writes FDS v4 files, exposes records as JSON or terminal
//! output, and keeps database selection explicit when multiple files exist.
//! Graphical and network-serving features are intentionally excluded from
//! this CLI-only package.

use std::{collections::BTreeMap, fs, path::{Path, PathBuf}, process::ExitCode};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use fds::{v4::{self, DbMeta, Kind, Record}, FdsError};
use serde_json::{json, Value};
use unicode_normalization::UnicodeNormalization;
/// Parsed options shared by all FDS commands.
#[derive(Parser)]
#[command(name="fds", version, about="FDS — a database for knowledge that AI systems consume")]
struct Cli {
    /// Database file (defaults to the only *.fds file in the current directory)
    #[arg(long, global=true)] db: Option<PathBuf>,
    /// Emit machine-readable JSON
    #[arg(long, global=true)] json: bool,
    #[command(subcommand)] command: Command,
}

#[derive(Subcommand)]
/// Commands exposed by the FDS CLI.
///
/// Core database operations are implemented in this package. Some advanced
/// FDS commands remain reserved and return a clear unsupported-command error.
enum Command {
    Init { #[arg(default_value="knowledge")] name: String },
    Status,
    Add { paths: Vec<PathBuf>, #[arg(short,long,default_value="default")] collection:String, #[arg(long)] lang:Option<String> },
    Fact { #[command(subcommand)] command: FactCommand },
    Proc { #[command(subcommand)] command: ProcCommand },
    Context { #[command(subcommand)] command: ContextCommand },
    Ls { #[arg(long)] kind:Option<String>, #[arg(long="type")] r#type:Option<String>, #[arg(short,long)] collection:Option<String> },
    Show { id:String },
    Rm { id:Option<String>, #[arg(long)] source:Option<String> },
    Query { #[arg(required=true)] text:Vec<String>, #[arg(short,long)] collection:Option<String>, #[arg(long,default_value_t=400)] budget:u32, #[arg(long)] links:bool, #[arg(long)] verified_by:Option<String> },
    Import { file:PathBuf, #[arg(short,long,default_value="default")] collection:String, #[arg(long)] dry_run:bool, #[arg(long)] skip_errors:bool },
    Embed { #[arg(long)] model:Option<String>, #[arg(long)] all:bool },
    Mcp { #[arg(long)] write:bool, #[arg(long="as")] agent:Option<String>, #[arg(long)] connect:Option<String> },
    Sector { #[command(subcommand)] command:SectorCommand },
    Log { summary:String, #[arg(long)] why:Option<String>, #[arg(long="file")] files:Vec<String>, #[arg(long)] by:Option<String>, #[arg(long)] role:Option<String>, #[arg(long)] status:Option<String>, #[arg(long)] to:Option<String> },
    Journal { #[command(subcommand)] command:JournalCommand },
    Policy { #[command(subcommand)] command:PolicyCommand },
    Check { #[arg(long)] subject:String, #[arg(long)] action:String, #[arg(long,allow_hyphen_values=true)] value:Option<String> },
    Guard { #[arg(long)] policies:PathBuf, #[arg(long)] agent:Option<String>, #[arg(trailing_var_arg=true,required=true)] command:Vec<String> },
    Audit { #[command(subcommand)] command:AuditCommand },
}
/// Operations for creating and importing question-and-answer facts.
#[derive(Subcommand)] enum FactCommand { Add { question:String, answer:String, #[arg(short,long,default_value="default")] collection:String, #[arg(long,default_value="manual")] source:String }, Import { file:PathBuf, #[arg(short,long,default_value="default")] collection:String } }
/// Operations for adding procedural records from JSON.
#[derive(Subcommand)] enum ProcCommand { Add { file:PathBuf, #[arg(short,long,default_value="default")] collection:String } }
/// Operations for named, longer-form context records.
#[derive(Subcommand)] enum ContextCommand { Set { name:String, #[arg(long)] instructions:Option<String>, #[arg(long)] file:Option<PathBuf>, #[arg(long="link")] links:Vec<String>, #[arg(short,long,default_value="default")] collection:String }, Ls, Show { name:String, #[arg(long)] known:Option<String> }, Import { file:PathBuf, #[arg(long)] name:Option<String>, #[arg(short,long,default_value="default")] collection:String }, Export { name:String } }
/// Operations reserved for the future sector-access interface.
#[derive(Subcommand)] enum SectorCommand { New { name:String, #[arg(long)] agent:bool, #[arg(long)] reads:Vec<String>, #[arg(long)] clearance:Vec<String> }, Ls, Key { name:String }, Promote { id:String, #[arg(long)] from:String, #[arg(long)] to:String } }
/// Operations reserved for journal export and publishing.
#[derive(Subcommand)] enum JournalCommand { Show { #[arg(long)] timeline:bool, #[arg(long)] md:bool }, Export { #[arg(long)] out:PathBuf }, Publish { #[arg(long)] repo:PathBuf } }
/// Operations reserved for structured policy rules.
#[derive(Subcommand)] enum PolicyCommand { Add { description:String, #[arg(long)] subject:String, #[arg(long)] action:String, #[arg(long)] rule:String, #[arg(long)] min:Option<f64>, #[arg(long)] max:Option<f64>, #[arg(long)] unit:Option<String> }, Ls }
/// Operations reserved for audit-log inspection and verification.
#[derive(Subcommand)] enum AuditCommand { Show, Verify }

/// Parse arguments, run the requested command, and return a process exit code.
fn main() -> ExitCode {
    match run(Cli::parse()) { Ok(code) => ExitCode::from(code), Err(e) => { eprintln!("fds: {e}"); ExitCode::FAILURE } }
}

/// Execute one CLI command against the selected database.
///
/// Mutating commands hold the writer lock from database read through save so
/// concurrent CLI processes cannot silently overwrite each other's updates.
fn run(cli:Cli)->Result<u8,String>{
    // Initialization creates a new file and therefore does not need a lookup.
    if let Command::Init{name}=&cli.command { return init(name); }
    let path=locate(cli.db.as_deref())?;
    // Keep the lock alive for the full read-modify-write command.
    let _write_guard = if is_write(&cli.command) { Some(lock_database(&path)?) } else { None };
    let (mut meta,mut records)=v4::read_db(&path).map_err(|e|format!("{}: {}",path.display(),explain(e)))?;
    match cli.command {
        Command::Status => { let mut collections=BTreeMap::<String,usize>::new();for r in &records{*collections.entry(r.collection.clone()).or_default()+=1;}let bytes=fs::metadata(&path).map(|m|m.len()).unwrap_or(0);let value=json!({"database":path,"bytes":bytes,"records":records.len(),"facts":records.iter().filter(|r|r.kind==Kind::Fact).count(),"procedures":records.iter().filter(|r|r.kind==Kind::Procedure).count(),"knowledge":records.iter().filter(|r|r.kind==Kind::Knowledge).count(),"embedded":records.iter().filter(|r|r.embedding.is_some()).count(),"collections":collections}); print_value(value,cli.json); }
        Command::Fact{command:FactCommand::Add{question,answer,collection,source}} => { let r=fact(&question,&answer,&collection,&source)?; upsert(&mut records,r.clone()); save(&path,&mut meta,&records)?; println!("{}",r.id); }
        Command::Fact{command:FactCommand::Import{file,collection}} => { let v:Value=read_json(&file)?; let rows=v.as_array().or_else(||v.get("entries").and_then(Value::as_array)).ok_or("expected an array of facts")?; let mut n=0; for row in rows { let q=row.get("question").or_else(||row.get("canonical_query")).and_then(Value::as_str).ok_or("fact is missing question")?; let a=row.get("answer").and_then(Value::as_str).ok_or("fact is missing answer")?; let source=row.get("source").and_then(Value::as_str).unwrap_or("import"); upsert(&mut records,fact(q,a,&collection,source)?); n+=1; } save(&path,&mut meta,&records)?; println!("{n} facts imported"); }
        Command::Proc{command:ProcCommand::Add{file,collection}} => { let v=read_json(&file)?; let rows=v.as_array().cloned().unwrap_or_else(||vec![v]); let mut staged=Vec::new(); for row in rows { let intent=row.get("intent").and_then(Value::as_str).ok_or("procedure is missing intent")?; let steps=row.get("steps").ok_or("procedure is missing steps")?; if !steps.is_array(){return Err("procedure steps must be an array".into())} let body=json!({"steps":steps}); staged.push(record(Kind::Procedure,&collection,Some(intent),body.to_string(),intent,&file.display().to_string(),"human","")); } for r in staged {upsert(&mut records,r)} save(&path,&mut meta,&records)?; println!("procedures added"); }
        Command::Ls{kind,r#type,collection} => { let out:Vec<_>=records.iter().filter(|r|kind.as_deref().is_none_or(|x|x==r.kind.as_str())).filter(|r|r#type.as_deref().is_none_or(|x|x==r.rtype)).filter(|r|collection.as_deref().is_none_or(|x|x==r.collection)).map(public_record).collect(); print_value(json!(out),cli.json); }
        Command::Show{id} => { let r=resolve(&records,&id)?; print_value(public_record(r),cli.json); }
        Command::Rm{id,source} => { let before=records.len(); records.retain(|r|if let Some(s)=&source {r.source!=*s} else {Some(r.id.as_str())!=id.as_deref()}); if before==records.len(){return Err("no matching record".into())} save(&path,&mut meta,&records)?; println!("removed {} record(s)",before-records.len()); }
        Command::Query{text,collection,budget,links,verified_by} => {
            let result = query_records(&records, &text.join(" "), collection.as_deref(), budget,
                links, verified_by.as_deref());
            if result["confidence"] == "none" { print_value(result, cli.json); return Ok(2); }
            print_value(result, cli.json);
        }
        Command::Context{command:ContextCommand::Set{name,instructions,file,links,collection}} => { let text=match(instructions,file){(Some(s),None)=>s,(None,Some(p))=>fs::read_to_string(p).map_err(|e|e.to_string())?,_=>return Err("provide exactly one of --instructions or --file".into())}; let mut r=record(Kind::Knowledge,&collection,None,text,&name,"manual","human","context"); r.id=format!("{:016x}",fnv(format!("context:{}",fold(&name)).as_bytes())); r.links=links; upsert(&mut records,r); save(&path,&mut meta,&records)?; println!("context saved: {name}"); }
        Command::Context{command:ContextCommand::Ls} => { let out:Vec<_>=records.iter().filter(|r|r.rtype=="context").map(public_record).collect(); print_value(json!(out),cli.json); }
        Command::Context{command:ContextCommand::Show{name,known}} => { let r=records.iter().find(|r|r.rtype=="context"&&r.title.eq_ignore_ascii_case(&name)).ok_or("context not found")?; if known.as_deref()==Some(r.id.as_str()){println!("unchanged");}else{print_value(public_record(r),cli.json);} }
        Command::Context{command:ContextCommand::Import{file,name,collection}} => { let text=fs::read_to_string(&file).map_err(|e|e.to_string())?; let title=name.or_else(||file.file_stem().and_then(|x|x.to_str()).map(str::to_string)).unwrap_or_else(||"context".into()); let mut r=record(Kind::Knowledge,&collection,None,text,&title,&file.display().to_string(),"human","context");r.id=format!("{:016x}",fnv(format!("context:{}",fold(&title)).as_bytes()));upsert(&mut records,r);save(&path,&mut meta,&records)?;println!("context imported: {title}"); }
        Command::Context{command:ContextCommand::Export{name}} => { let r=records.iter().find(|r|r.rtype=="context"&&r.title.eq_ignore_ascii_case(&name)).ok_or("context not found")?; println!("{}",r.body); }
        Command::Add{paths,collection,..} => { let mut n=0; for p in paths { if p.is_dir(){ for e in fs::read_dir(p).map_err(|e|e.to_string())? {let f=e.map_err(|e|e.to_string())?.path(); if f.is_file(){n+=ingest_text(&f,&collection,&mut records)?}} }else{n+=ingest_text(&p,&collection,&mut records)?} } save(&path,&mut meta,&records)?; println!("{n} knowledge records added"); }
        Command::Import{file,collection,dry_run,..} => { let v=read_json(&file)?; let rows=v.as_array().cloned().unwrap_or_else(||vec![v]); if dry_run {println!("{} rows inspected",rows.len());} else {for row in rows { if let (Some(q),Some(a))=(row.get("question").and_then(Value::as_str),row.get("answer").and_then(Value::as_str)){upsert(&mut records,fact(q,a,&collection,"import")?);} } save(&path,&mut meta,&records)?; println!("import complete");} }
        Command::Embed{..}|Command::Mcp{..}|Command::Sector{..}|Command::Log{..}|Command::Journal{..}|Command::Policy{..}|Command::Check{..}|Command::Guard{..}|Command::Audit{..} => return Err("this command group is part of the FDS interface and is not implemented in this initial clean build yet".into()),
        Command::Init{..}=>unreachable!(),
    }
    Ok(0)
}

/// Create an empty FDS v4 database, adding the file extension when omitted.
fn init(name:&str)->Result<u8,String>{let mut p=PathBuf::from(name); if p.extension().is_none(){p.set_extension("fds");} if p.exists(){return Err(format!("{} already exists",p.display()))} let today=today(); let meta=DbMeta{name:p.file_stem().and_then(|x|x.to_str()).unwrap_or("knowledge").into(),created:today.clone(),updated:today,embedding_model:String::new(),embedding_dims:0}; v4::write_db(&p,&meta,&[]).map_err(|e|format!("{e:?}"))?; println!("created {}",p.display()); Ok(0)}
/// Select the requested database or infer it only when exactly one `.fds` file exists.
fn locate(explicit:Option<&Path>)->Result<PathBuf,String>{if let Some(p)=explicit{return Ok(p.into())} let mut files:Vec<_>=fs::read_dir(".").map_err(|e|e.to_string())?.filter_map(Result::ok).map(|e|e.path()).filter(|p|p.is_file()&&p.extension().is_some_and(|x|x=="fds")).collect(); files.sort(); match files.len(){0=>Err("no database here; run `fds init` or pass --db".into()),1=>Ok(files.remove(0)),_=>Err("multiple .fds databases found; select one with --db".into())}}
/// Build a record with a stable content-derived ID and current metadata.
///
/// Facts and procedures use their normalized question/intent as the ID seed;
/// knowledge records use their source identifier.
fn record(kind:Kind,c:&str,q:Option<&str>,body:String,title:&str,source:&str,verified:&str,rtype:&str)->Record{let seed=match kind{Kind::Fact=>format!("fact:{}",canonical(q.unwrap_or(""))),Kind::Procedure=>format!("procedure:{}",canonical(q.unwrap_or(""))),Kind::Knowledge=>format!("knowledge:{source}#0")}; let id=format!("{:016x}",fnv(seed.as_bytes())); Record{id,kind,collection:c.into(),question:q.map(canonical),tokens:body.split_whitespace().count() as u32,body,title:title.into(),source:source.into(),verified_by:verified.into(),last_verified:today(),embedding:None,rtype:rtype.into(),attrs:BTreeMap::new(),links:vec![]}}
/// Validate and construct a non-empty fact record.
fn fact(q:&str,a:&str,c:&str,s:&str)->Result<Record,String>{if q.trim().is_empty()||a.trim().is_empty(){return Err("a fact needs both a question and an answer".into())} Ok(record(Kind::Fact,c,Some(q),a.trim().into(),q,s,"human",""))}
/// Replace any existing record with the same stable ID.
fn upsert(rs:&mut Vec<Record>,r:Record){rs.retain(|x|x.id!=r.id);rs.push(r)}
/// Update database metadata and persist the complete record set.
fn save(p:&Path,m:&mut DbMeta,r:&[Record])->Result<(),String>{m.updated=today();v4::write_db(p,m,r).map_err(|e|format!("{e:?}"))}
/// Read a UTF-8 text file and add it as one knowledge record.
fn ingest_text(p:&Path,c:&str,rs:&mut Vec<Record>)->Result<usize,String>{let text=fs::read_to_string(p).map_err(|e|format!("{}: {e}",p.display()))?;let title=p.file_name().and_then(|x|x.to_str()).unwrap_or("document");let r=record(Kind::Knowledge,c,None,text,title,&p.display().to_string(),"ingest","");upsert(rs,r);Ok(1)}
/// Deserialize a JSON file and include its path in any parse error.
fn read_json(p:&Path)->Result<Value,String>{serde_json::from_slice(&fs::read(p).map_err(|e|format!("{}: {e}",p.display()))?).map_err(|e|format!("{}: {e}",p.display()))}
/// Find a full record by ID or by an ID prefix.
fn resolve<'a>(rs:&'a[Record],id:&str)->Result<&'a Record,String>{rs.iter().find(|r|r.id==id||r.id.starts_with(id)).ok_or_else(||format!("record not found: {id}"))}
/// Convert an internal record into the stable JSON shape used by CLI output.
fn public_record(r:&Record)->Value{json!({"id":r.id,"kind":r.kind.as_str(),"collection":r.collection,"question":r.question,"title":r.title,"body":r.body,"source":r.source,"verified_by":r.verified_by,"last_verified":r.last_verified,"tokens":r.tokens,"type":r.rtype,"attrs":r.attrs,"links":r.links,"embedding_dims":r.embedding.as_ref().map(Vec::len)})}
fn print_value(v:Value,json_out:bool){
    if json_out { println!("{}",serde_json::to_string_pretty(&v).unwrap()); return; }
    if let Some(results)=v.get("results").and_then(Value::as_array) {
        println!("confidence: {}",v.get("confidence").and_then(Value::as_str).unwrap_or("none"));
        for row in results { print_record_line(row); }
    } else if let Some(rows)=v.as_array() {
        if rows.is_empty(){println!("(no records)");}
        for row in rows { print_record_line(row); }
    } else if v.get("id").is_some() { print_record_line(&v); }
    else { println!("{}",serde_json::to_string_pretty(&v).unwrap()); }
}
fn print_record_line(row:&Value){
    let id=row.get("id").and_then(Value::as_str).unwrap_or("-");
    let kind=row.get("kind").and_then(Value::as_str).unwrap_or("record");
    let name=row.get("question").and_then(Value::as_str).or_else(||row.get("title").and_then(Value::as_str)).unwrap_or("-");
    let body=row.get("body").and_then(Value::as_str).unwrap_or("");
    println!("{id}\t{kind}\t{name}\t{body}");
}
fn fold(s:&str)->String{s.nfc().collect::<String>().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")}
fn fnv(bytes:&[u8])->u64{bytes.iter().fold(0xcbf29ce484222325u64,|h,b|(h^u64::from(*b)).wrapping_mul(0x100000001b3))}
fn today()->String{use std::time::{SystemTime,UNIX_EPOCH};let days=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()/86400;let z=days as i64+719468;let era=if z>=0{z}else{z-146096}/146097;let doe=z-era*146097;let yoe=(doe-doe/1460+doe/36524-doe/146096)/365;let y=yoe+era*400;let doy=doe-(365*yoe+yoe/4-yoe/100);let mp=(5*doy+2)/153;let d=doy-(153*mp+2)/5+1;let m=mp+if mp<10{3}else{-9};format!("{:04}-{:02}-{:02}",y+if m<=2{1}else{0},m,d)}
fn explain(e:FdsError)->String{format!("{e:?}")}

fn is_write(command:&Command)->bool {
    matches!(command,
        Command::Add{..}|Command::Fact{..}|Command::Proc{..}|Command::Rm{..}|Command::Import{..}
        |Command::Context{command:ContextCommand::Set{..}|ContextCommand::Import{..}}
    )
}

/// Hold the writer lock from read through atomic replacement. This prevents
/// two FDS processes from silently overwriting each other's changes.
fn lock_database(path:&Path)->Result<fs::File,String>{
    let mut lock_name=path.as_os_str().to_owned(); lock_name.push(".lock");
    let lock_path=PathBuf::from(lock_name);
    let file=fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(&lock_path).map_err(|e|format!("{}: {e}",lock_path.display()))?;
    let start=Instant::now();
    loop {
        match file.try_lock() {
            Ok(())=>return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed()<Duration::from_secs(30)=>std::thread::sleep(Duration::from_millis(25)),
            Err(std::fs::TryLockError::WouldBlock)=>return Err(format!("{} is busy; writer lock timed out",path.display())),
            Err(std::fs::TryLockError::Error(e))=>return Err(format!("cannot lock {}: {e}",path.display())),
        }
    }
}

/// Two-stage FDS retrieval: precise compiled answers first, lexical retrieval
/// second. The returned shape stays stable for CLI and future API callers.
fn query_records(records:&[Record], query:&str, collection:Option<&str>, budget:u32,
                 include_links:bool, verified_by:Option<&str>)->Value {
    let visible = |r: &&Record| r.rtype != "context"
        && collection.is_none_or(|c| c == r.collection)
        && verified_by.is_none_or(|list| list.split(',').map(str::trim).any(|w| {
            if let Some(prefix) = w.strip_suffix(':') { r.verified_by.starts_with(prefix) }
            else { r.verified_by == w }
        }));
    // Prefer an exact normalized fact/procedure question before fuzzy retrieval.
    let qcanon = canonical(query);
    let mut compiled: Vec<&Record> = records.iter().filter(visible)
        .filter(|r| matches!(r.kind, Kind::Fact | Kind::Procedure))
        .filter(|r| r.question.as_deref().is_some_and(|stored| {
            canonical(stored) == qcanon || token_key(stored) == token_key(&qcanon)
        })).collect();
    if let Some(r) = compiled.pop() {
        let mut result = public_record(r);
        result["confidence"] = json!("exact");
        result["match"] = json!("compiled");
        return json!({"confidence":"exact","results":[result]});
    }

    // Fall back to a small BM25-style lexical ranking when there is no exact hit.
    let eligible:Vec<&Record> = records.iter().filter(visible).collect();
    let indexed:Vec<Vec<String>> = eligible.iter().map(|r| {
        let mut text = format!("{} {} {}", r.title, r.title, r.title);
        if let Some(q) = &r.question { text.push_str(&format!(" {q} {q} {q}")); }
        text.push_str(&format!(" {} {}", r.body, r.source));
        search_terms(&text)
    }).collect();
    let n = eligible.len() as f64;
    let avg_len = indexed.iter().map(Vec::len).sum::<usize>() as f64 / n.max(1.0);
    let terms = search_terms(query);
    if terms.is_empty() { return json!({"confidence":"none","results":[]}); }
    let mut scored=Vec::new();
    for (i, doc) in indexed.iter().enumerate() {
        let dl=doc.len() as f64;
        let mut score=0.0;
        for term in &terms {
            let tf=doc.iter().filter(|x|*x==term).count() as f64;
            if tf==0.0 {continue}
            let df=indexed.iter().filter(|d|d.iter().any(|x|x==term)).count() as f64;
            let idf=((n-df+0.5)/(df+0.5)+1.0).ln();
            score += idf * (tf*2.5)/(tf+1.5*(1.0-0.75+0.75*dl/avg_len.max(1.0)));
        }
        if score>0.0 {scored.push((i,score));}
    }
    if scored.is_empty() {return json!({"confidence":"none","results":[]});}
    scored.sort_by(|a,b|b.1.total_cmp(&a.1));
    scored.truncate(5);
    let first=eligible[scored[0].0];
    let first_terms=search_terms(&format!("{} {} {} {}",first.title,first.body,first.source,first.question.as_deref().unwrap_or("")));
    // These labels describe term coverage, not factual certainty or verification.
    let confidence=if terms.iter().all(|t|first_terms.contains(t)){"strong"}else{"weak"};
    let mut result_ids=std::collections::HashSet::new();
    let mut result_rows=Vec::new();
    let mut used=0u32;
    for (i,_) in scored {
        let r=eligible[i];
        if result_ids.insert(r.id.as_str()) && (result_rows.is_empty() || used+r.tokens<=budget) {
            used=used.saturating_add(r.tokens);
            let mut row=public_record(r); row["match"]=json!("keyword"); result_rows.push(row);
        }
    }
    if include_links {
        let base:Vec<&Record>=records.iter().filter(visible).collect();
        let roots:Vec<&Record>=result_rows.iter().filter_map(|row|records.iter().find(|r|row["id"]==r.id)).collect();
        for root in roots { for target in &root.links {
            let target_c=canonical(target);
            if let Some(r)=base.iter().find(|r|r.question.as_deref().is_some_and(|q|canonical(q)==target_c)||canonical(&r.title)==target_c) {
                if result_ids.insert(r.id.as_str()) && used+r.tokens<=budget { used+=r.tokens; let mut row=public_record(r); row["match"]=json!("link"); result_rows.push(row); }
            }
        }}
    }
    json!({"confidence":confidence,"results":result_rows})
}

fn canonical(s:&str)->String {
    s.nfc().collect::<String>().to_lowercase().chars().map(|c| if c.is_alphanumeric()||c.is_whitespace()||is_mark(c){c}else{' '}).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}
fn token_key(s:&str)->Vec<String>{let mut t=content_tokens(s);t.sort();t.dedup();t}
fn content_tokens(s:&str)->Vec<String>{search_terms(s)}
fn search_terms(s:&str)->Vec<String>{
    const STOPWORDS:&str=include_str!("../stopwords.txt");
    let folded=search_fold(s);
    folded.split(|c:char|!c.is_alphanumeric()&&!is_mark(c)).filter(|t|!t.is_empty()&&!STOPWORDS.lines().any(|word|word==*t)).map(str::to_string).collect()
}
fn is_mark(c:char)->bool{matches!(c as u32,0x0300..=0x036f|0x1ab0..=0x1aff|0x1dc0..=0x1dff|0x20d0..=0x20ff|0xfe20..=0xfe2f)}
fn search_fold(s:&str)->String {
    let compatibility:String=s.nfkc().flat_map(char::to_lowercase).collect();
    let compatibility=compatibility.replace('ß',"ss").replace('ς',"σ").replace('ı',"i").replace('ł',"l").replace('ø',"o").replace('đ',"d").replace('ð',"d").replace('ħ',"h").replace('ŧ',"t").replace('æ',"ae").replace('œ',"oe").replace('þ',"th");
    compatibility.nfd().filter(|c|!is_mark(*c)).collect::<String>().nfc().collect()
}
