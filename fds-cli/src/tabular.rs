//! Reading tables -- CSV, TSV, JSON, JSON Lines -- into import rows (SPEC §8.3).
//!
//! The column names FDS recognises, in English and Spanish, live here and
//! only here: the command line uses them directly, and the manager receives
//! them from the server, so the two cannot come to disagree about what a
//! column called "Pregunta" means.

use serde_json::{json, Map, Value};

/// Field -> the normalised header names that mean it. A header is
/// normalised by `norm`: lower case, accents dropped, only letters and digits.
pub const ALIASES: &[(&str, &[&str])] = &[
    ("kind", &["kind", "clase"]),
    ("type", &["type", "tipo"]),
    ("question", &["question", "pregunta", "q", "canonicalquery", "query", "consulta"]),
    ("answer", &["answer", "respuesta", "a", "value", "valor"]),
    ("intent", &["intent", "intencion", "tarea", "task"]),
    ("title", &["title", "titulo", "name", "nombre"]),
    ("steps", &["steps", "pasos"]),
    ("command", &["command", "comando", "cmd"]),
    ("shell", &["shell", "consola"]),
    ("attrs", &["attrs", "atributos", "attributes"]),
    ("links", &["links", "enlaces", "vinculos"]),
    ("collection", &["collection", "coleccion", "category", "categoria"]),
    ("source", &["source", "fuente", "origen"]),
    ("verified_by", &["verifiedby", "verificadopor"]),
    ("last_verified", &["lastverified", "fecha", "date", "fechaverificacion", "fechadeverificacion"]),
];

/// The fields each kind of row may carry. Anything else in a row is dropped,
/// so a stray column never lands inside a procedure's body.
pub const FIELDS: &[(&str, &[&str])] = &[
    ("fact", &["question", "answer", "type", "attrs", "links", "collection", "source", "verified_by", "last_verified"]),
    ("procedure", &["intent", "title", "steps", "command", "shell", "type", "attrs", "links", "collection", "source", "verified_by", "last_verified"]),
    ("context", &["title", "answer", "links", "collection", "source", "verified_by", "last_verified"]),
];

/// Values that make a column called "tipo" or "type" the kind, not the free type.
pub const KIND_WORDS: &[&str] = &["fact", "hecho", "procedure", "procedimiento", "context", "contexto"];

/// What the manager is handed: the same three tables, as JSON.
pub fn client_json() -> String {
    let map = |t: &[(&str, &[&str])]| -> Value {
        Value::Object(t.iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
    };
    json!({ "aliases": map(ALIASES), "fields": map(FIELDS), "kind_words": KIND_WORDS }).to_string()
}

/// Lower case, Latin accents folded, only letters and digits: "Categoría" and
/// "categoria" and "CATEGORIA" are one header.
pub fn norm(s: &str) -> String {
    s.chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            other => other,
        })
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// UTF-8 if the bytes are UTF-8, else Windows-1252 -- which is what Excel in
/// Spanish, French or German writes. A leading byte-order mark is dropped.
pub fn decode(bytes: &[u8]) -> (String, &'static str) {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), "UTF-8"),
        Err(_) => (bytes.iter().map(|&b| cp1252(b)).collect(), "Windows-1252"),
    }
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
        '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// The delimiter used most on the header line, outside quotes: `;` from a
/// continental Excel, tab from a paste, `,` elsewhere.
pub fn detect_delimiter(text: &str) -> char {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let count = |d: char| {
        let mut n = 0;
        let mut q = false;
        for c in line.chars() {
            if c == '"' {
                q = !q;
            } else if !q && c == d {
                n += 1;
            }
        }
        n
    };
    [',', ';', '\t', '|'].into_iter().max_by_key(|&d| count(d)).unwrap_or(',')
}

/// RFC 4180: quoted cells may hold the delimiter, doubled quotes and line
/// breaks. Undoes the export's formula guard (SPEC §8.3): one apostrophe before
/// `=`, `+`, `-`, `@`, tab or carriage return is removed. Wholly empty rows
/// are dropped.
pub fn parse_csv(text: &str, delim: char) -> Vec<Vec<String>> {
    let unguard = |v: String| match v.strip_prefix('\'') {
        Some(rest) if rest.starts_with(['=', '+', '-', '@', '\t', '\r']) => rest.to_string(),
        _ => v,
    };
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            (true, '"') => quoted = false,
            (true, c) => cell.push(c),
            (false, '"') => quoted = true,
            (false, c) if c == delim => row.push(unguard(std::mem::take(&mut cell))),
            (false, '\r') => {}
            (false, '\n') => {
                row.push(unguard(std::mem::take(&mut cell)));
                rows.push(std::mem::take(&mut row));
            }
            (false, c) => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(unguard(cell));
        rows.push(row);
    }
    rows.retain(|r| r.iter().any(|c| !c.trim().is_empty()));
    rows
}

/// Which column holds which field, from the header names -- including the
/// "tipo"/"type" column, decided by what it holds.
pub fn map_columns(headers: &[String], rows: &[Vec<String>]) -> Vec<(String, usize)> {
    let n: Vec<String> = headers.iter().map(|h| norm(h)).collect();
    let mut out: Vec<(String, usize)> = Vec::new();
    for (field, aliases) in ALIASES {
        if let Some(i) = n.iter().position(|h| aliases.contains(&h.as_str())) {
            out.push((field.to_string(), i));
        }
    }
    let has = |f: &str| out.iter().any(|(k, _)| k == f);
    if !has("kind") {
        if let Some(pos) = out.iter().position(|(k, _)| k == "type") {
            let col = out[pos].1;
            let vals: Vec<String> = rows.iter().take(50).filter_map(|r| r.get(col)).map(|v| norm(v)).filter(|v| !v.is_empty()).collect();
            let kindish = vals.iter().filter(|v| KIND_WORDS.contains(&v.as_str())).count();
            if !vals.is_empty() && kindish * 5 >= vals.len() * 4 {
                out[pos].0 = "kind".into();
            }
        }
    }
    out
}

fn row_kind(raw: &str) -> String {
    match norm(raw).as_str() {
        "fact" | "hecho" => "fact".into(),
        "procedure" | "procedimiento" => "procedure".into(),
        "context" | "contexto" => "context".into(),
        _ => raw.trim().to_string(),
    }
}

/// Import rows from a table. `default_kind` applies when there is no kind
/// column: an explicit choice, or, left out, `procedure` for a table with an
/// intent, steps or command and no answer, else `fact`.
pub fn rows_from_table(headers: &[String], rows: &[Vec<String>], default_kind: Option<&str>) -> Vec<Value> {
    let cols = map_columns(headers, rows);
    let col = |f: &str| cols.iter().find(|(k, _)| k == f).map(|(_, i)| *i);
    let inferred = default_kind.map(str::to_string).unwrap_or_else(|| {
        let procish = col("intent").is_some() || col("steps").is_some() || col("command").is_some();
        if procish && col("answer").is_none() { "procedure".into() } else { "fact".into() }
    });
    rows.iter()
        .map(|r| {
            let kind = match col("kind") {
                Some(i) => row_kind(r.get(i).map(String::as_str).unwrap_or("")),
                None => inferred.clone(),
            };
            let mut o = Map::new();
            o.insert("kind".into(), json!(kind));
            let allowed: Vec<&str> = FIELDS
                .iter()
                .find(|(k, _)| *k == kind)
                .map(|(_, f)| f.to_vec())
                .unwrap_or_else(|| ALIASES.iter().map(|(f, _)| *f).filter(|f| *f != "kind").collect());
            for f in allowed {
                if let Some(v) = col(f).and_then(|i| r.get(i)).map(|v| v.trim()).filter(|v| !v.is_empty()) {
                    o.insert(f.into(), json!(v));
                }
            }
            Value::Object(o)
        })
        .collect()
}

/// Rows from a JSON document: an array of objects, or an object holding one
/// under `entries` or `rows`. Objects already in FDS's shape pass as they are.
pub fn rows_from_json(text: &str) -> Result<Vec<Value>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let items = match v {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("entries").or_else(|| o.remove("rows")) {
            Some(Value::Array(a)) => a,
            _ => vec![Value::Object(o)],
        },
        _ => return Err("expected an array of objects".into()),
    };
    if !items.iter().all(Value::is_object) {
        return Err("expected an array of objects".into());
    }
    Ok(items)
}

/// Rows from JSON Lines: one object per non-empty line.
pub fn rows_from_jsonl(text: &str) -> Result<Vec<Value>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| serde_json::from_str::<Value>(l).map_err(|e| format!("line {}: {e}", i + 1)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spanish_excel_csv_reads_as_intended() {
        // Windows-1252 bytes, `;`, a quoted cell holding the delimiter, an empty row.
        let text = "Pregunta;Respuesta;Categor\u{ED}a\r\n\u{BF}Pin 4 del NE555?;\"RESET; activo en bajo\";electr\u{F3}nica\r\n;;\r\n";
        let bytes: Vec<u8> = text.chars().map(|c| c as u32 as u8).collect();
        let (decoded, enc) = decode(&bytes);
        assert_eq!(enc, "Windows-1252");
        assert_eq!(detect_delimiter(&decoded), ';');
        let table = parse_csv(&decoded, ';');
        assert_eq!(table.len(), 2, "the empty row is dropped");
        let rows = rows_from_table(&table[0], &table[1..], None);
        assert_eq!(rows[0], json!({"kind": "fact", "question": "¿Pin 4 del NE555?", "answer": "RESET; activo en bajo", "collection": "electrónica"}));
    }

    #[test]
    fn a_tipo_column_is_the_kind_only_when_it_holds_kinds() {
        let h = vec!["Tipo".to_string(), "Pregunta".to_string(), "Respuesta".to_string()];
        let kinds = vec![vec!["hecho".into(), "q".into(), "a".into()]];
        assert_eq!(rows_from_table(&h, &kinds, None)[0]["kind"], "fact");
        let free = vec![vec!["network".into(), "q".into(), "a".into()]];
        let r = &rows_from_table(&h, &free, None)[0];
        assert_eq!((r["kind"].as_str(), r["type"].as_str()), (Some("fact"), Some("network")));
    }

    #[test]
    fn stray_columns_never_reach_a_procedure_and_formulas_are_unguarded() {
        let t = parse_csv("intent,command,answer,note\nflush dns,ipconfig /flushdns,oops,x\nvolts,'-5 V,,\n", ',');
        let rows = rows_from_table(&t[0], &t[1..], Some("procedure"));
        assert_eq!(rows[0], json!({"kind": "procedure", "intent": "flush dns", "command": "ipconfig /flushdns"}));
        assert_eq!(rows[1]["command"], "-5 V");
    }
}
