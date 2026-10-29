//! Normalisation (SPEC.md §4.1), stable identifiers (§3.1), dates.

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use unicode_normalization::char::{canonical_combining_class, is_combining_mark};
use unicode_normalization::UnicodeNormalization;

const STOPWORDS: &[&str] = &[
    // English
    "a",
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "by",
    "did",
    "do",
    "does",
    "doing",
    "down",
    "during",
    "each",
    "few",
    "for",
    "from",
    "further",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "just",
    "me",
    "more",
    "most",
    "my",
    "myself",
    "now",
    "of",
    "off",
    "on",
    "once",
    "only",
    "or",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "same",
    "she",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "through",
    "to",
    "too",
    "under",
    "until",
    "up",
    "very",
    "was",
    "we",
    "were",
    "what",
    "while",
    "with",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
    // Spanish
    "el",
    "la",
    "los",
    "las",
    "un",
    "una",
    "unos",
    "unas",
    "de",
    "del",
    "al",
    "y",
    "o",
    "u",
    "que",
    "en",
    "es",
    "son",
    "por",
    "para",
    "con",
    "se",
    "su",
    "sus",
    "lo",
    "le",
    "les",
    "como",
    "qué",
    "cuando",
    "donde",
    "este",
    "esta",
    "estos",
    "estas",
    "ese",
    "esa",
    "esos",
    "esas",
    "hay",
    "ser",
    "está",
    "están",
    "fue",
    "era",
    "mi",
    "mis",
    "tu",
    "tus",
    "me",
    "te",
    "nos",
    "ya",
    "muy",
    "más",
    "pero",
    "si",
    "también",
    "entre",
    "sobre",
];

fn stopwords() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STOPWORDS.iter().copied().collect())
}

fn punct_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[^\p{L}\p{N}\p{M}\s]").unwrap())
}

fn space_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap())
}

/// A run of search terms starts at a letter or a number; marks may follow.
fn term_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\p{L}\p{N}][\p{L}\p{N}\p{M}]*").unwrap())
}

fn token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\p{L}\p{N}\p{M}]+").unwrap())
}

/// Lower case, then Unicode normalisation form NFC, so that a letter and the
/// same letter written as a base plus a combining accent are one text.
fn fold(text: &str) -> String {
    text.to_lowercase().nfc().collect()
}

/// Lowercase, strip punctuation, collapse whitespace. Unicode-aware, so
/// accented Spanish letters survive, whichever way they were typed.
pub fn canonical(text: &str) -> String {
    let lowered = fold(text);
    let stripped = punct_re().replace_all(&lowered, " ");
    space_re().replace_all(&stripped, " ").trim().to_string()
}

/// Alphanumeric tokens of the canonical form, stopwords removed.
pub fn content_tokens(text: &str) -> Vec<String> {
    let lowered = fold(text);
    token_re()
        .find_iter(&lowered)
        .map(|m| m.as_str().to_string())
        .filter(|t| !stopwords().contains(t.as_str()))
        .collect()
}

/// Characters that are deleted outright by the search fold: invisible format
/// characters that carry no letter (soft hyphen, zero-width joiners, bidi
/// controls, word joiner, byte-order mark). Deleting them, instead of
/// treating them as separators, keeps a Persian word typed with or without
/// its half-space one term.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{200C}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

/// Combining marks that are removed by the search fold (SPEC.md §4.1): the
/// general diacritics, the optional vowel and recitation marks of Arabic and
/// Hebrew, and the variation selectors. The marks of Indic, Thai, Lao, Khmer
/// and Myanmar scripts are part of the word and are not in this list.
/// `base` is the letter the mark sits on: on a Cyrillic letter only the stress
/// marks (U+0300, U+0301) and the diaeresis of `е` go, so `й`, `ї` and `ў` stay
/// letters of their own while `ё` and `е` are one.
fn is_removable_mark(c: char, base: char) -> bool {
    match c {
        '\u{0300}'..='\u{036F}' => {
            if ('\u{0400}'..='\u{052F}').contains(&base) {
                c == '\u{0300}' || c == '\u{0301}' || (base == 'е' && c == '\u{0308}')
            } else {
                true
            }
        }
        '\u{0591}'..='\u{05BD}'
        | '\u{05BF}'
        | '\u{05C1}'..='\u{05C2}'
        | '\u{05C4}'..='\u{05C5}'
        | '\u{05C7}'
        | '\u{0610}'..='\u{061A}'
        | '\u{0640}'
        | '\u{064B}'..='\u{065F}'
        | '\u{0670}'
        | '\u{06D6}'..='\u{06DC}'
        | '\u{06DF}'..='\u{06E4}'
        | '\u{06E7}'..='\u{06E8}'
        | '\u{06EA}'..='\u{06ED}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20FF}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{FE20}'..='\u{FE2F}'
        | '\u{E0100}'..='\u{E01EF}' => true,
        _ => false,
    }
}

/// Letters that have no decomposition into a base and a mark but that a
/// reader of the plain-ASCII spelling would match: `ß` is `ss`, final sigma is
/// sigma, dotless `ı` is `i`, `ł`, `ø`, `đ`, `ð`, `ħ`, `ŧ` are their plain
/// letter, `æ`, `œ`, `þ` are `ae`, `oe`, `th`, and the five final forms of
/// Hebrew are the ordinary letters. (Applied after lower-casing.)
fn push_mapped(out: &mut String, c: char) {
    match c {
        'ß' => out.push_str("ss"),
        'ς' => out.push('σ'),
        'ı' => out.push('i'),
        'ł' => out.push('l'),
        'ø' => out.push('o'),
        'đ' | 'ð' => out.push('d'),
        'ħ' => out.push('h'),
        'ŧ' => out.push('t'),
        'æ' => out.push_str("ae"),
        'œ' => out.push_str("oe"),
        'þ' => out.push_str("th"),
        'ך' => out.push('כ'),
        'ם' => out.push('מ'),
        'ן' => out.push('נ'),
        'ף' => out.push('פ'),
        'ץ' => out.push('צ'),
        c => out.push(c),
    }
}

/// The search fold (SPEC.md §4.1): what makes `Straße`, `STRASSE` and
/// `ｓｔｒａｓｓｅ` one text, `atención` and `atencion` one text, and the
/// precomposed and the decomposed spelling of a letter one text. Compatibility
/// normalisation (NFKC), lower case, the letter replacements above, invisible
/// characters deleted, decomposition (NFD), the removable marks dropped,
/// composition (NFC). Only the matching uses it; stored text is never changed.
pub fn search_fold(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    let mut pre = String::with_capacity(text.len());
    for c in text.nfkc() {
        if is_invisible(c) {
            continue;
        }
        for l in c.to_lowercase() {
            push_mapped(&mut pre, l);
        }
    }
    let mut out = String::with_capacity(pre.len());
    let mut base = '\0';
    for c in pre.nfd() {
        if is_removable_mark(c, base) {
            continue;
        }
        if canonical_combining_class(c) == 0 {
            base = c;
        }
        out.push(c);
    }
    out.nfc().collect()
}

fn folded_stopwords() -> &'static HashSet<String> {
    static SET: OnceLock<HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| STOPWORDS.iter().map(|s| search_fold(s)).collect())
}

/// Scripts written without spaces between words (SPEC.md §4.1, "search
/// terms"): Han and the Japanese kana, Hangul syllables, Bopomofo, Thai, Lao,
/// Myanmar and Khmer. A run of these characters cannot be split into words
/// without a dictionary, so it is indexed as overlapping pairs.
fn is_unspaced(c: char) -> bool {
    matches!(c,
        '\u{0E00}'..='\u{0EFF}'
        | '\u{1000}'..='\u{109F}'
        | '\u{1780}'..='\u{17FF}'
        | '\u{2E80}'..='\u{2FDF}'
        | '\u{3000}'..='\u{312F}'
        | '\u{31A0}'..='\u{31BF}'
        | '\u{31F0}'..='\u{31FF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{AC00}'..='\u{D7AF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{3134F}')
}

/// The terms of one run of letters, numbers and marks of the folded text.
/// A run without unspaced-script characters is one term. In a run with them,
/// each maximal stretch of such characters is cut into units (a character with
/// the marks that follow it) and gives, for unit i, the unit and then the pair
/// of units i and i+1; the other stretches stay whole terms.
fn push_run_terms(run: &str, out: &mut Vec<String>) {
    if !run.chars().any(is_unspaced) {
        out.push(run.to_string());
        return;
    }
    let mut word = String::new();
    let mut units: Vec<String> = Vec::new();
    let flush_units = |units: &mut Vec<String>, out: &mut Vec<String>| {
        for i in 0..units.len() {
            out.push(units[i].clone());
            if i + 1 < units.len() {
                out.push(format!("{}{}", units[i], units[i + 1]));
            }
        }
        units.clear();
    };
    let mut in_unspaced = false;
    for c in run.chars() {
        let mark = is_combining_mark(c);
        // A mark takes the class of the character before it.
        let unspaced = if mark { in_unspaced } else { is_unspaced(c) };
        if unspaced {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            match units.last_mut() {
                Some(last) if mark => last.push(c),
                _ => units.push(c.to_string()),
            }
        } else {
            flush_units(&mut units, out);
            word.push(c);
        }
        in_unspaced = unspaced;
    }
    flush_units(&mut units, out);
    if !word.is_empty() {
        out.push(word);
    }
}

/// The terms stage 2 matches on (SPEC.md §4.1, "search terms"): the runs of
/// letters, numbers and marks of the search fold (a run starts at a letter or a
/// number), the runs of unspaced scripts cut into units and pairs, without the
/// stopwords (folded the same way, so `cual` is a stopword as `cuál` is).
/// A query without accents and a record with them are one text.
pub fn search_terms(text: &str) -> Vec<String> {
    let folded = search_fold(text);
    let mut out = Vec::new();
    for m in term_re().find_iter(&folded) {
        push_run_terms(m.as_str(), &mut out);
    }
    out.retain(|t| !folded_stopwords().contains(t));
    out
}

/// Sorted, de-duplicated content tokens: the key for "normalized" matching.
pub fn token_set_key(text: &str) -> Vec<String> {
    let mut v = content_tokens(text);
    v.sort();
    v.dedup();
    v
}

/// 64-bit FNV-1a as 16 hex characters. Deterministic across platforms and
/// runs, which is what makes re-ingestion replace rather than duplicate.
pub fn stable_id(input: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Today's UTC date as YYYY-MM-DD, without a date library.
pub fn today() -> String {
    now()[..10].to_string()
}

/// The current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

pub fn word_count(text: &str) -> u32 {
    text.split_whitespace().count() as u32
}

/// Targets of `[[links]]` in a text, as canonical keys, in order, without
/// repeats. Obsidian's syntax: `[[target]]`, or `[[target|shown text]]`
/// where only the target counts. Links inside fenced or inline code are
/// text, not links -- a README that documents the syntax must not link.
pub fn wikilinks(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Blank out inline code spans so their contents cannot match.
        let mut visible = String::with_capacity(line.len());
        let mut in_code = false;
        for c in line.chars() {
            if c == '`' {
                in_code = !in_code;
            }
            visible.push(if in_code || c == '`' { ' ' } else { c });
        }
        let mut rest = visible.as_str();
        while let Some(start) = rest.find("[[") {
            rest = &rest[start + 2..];
            let Some(end) = rest.find("]]") else { break };
            let inner = &rest[..end];
            rest = &rest[end + 2..];
            let target = inner.split('|').next().unwrap_or("").trim();
            let key = canonical(target);
            if !key.is_empty() && !inner.contains('[') && !out.contains(&key) {
                out.push(key);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_strips_punctuation_and_case() {
        assert_eq!(
            canonical("What is the LM317's  reference, voltage?"),
            "what is the lm317 s reference voltage"
        );
    }

    #[test]
    fn spanish_accents_survive() {
        assert_eq!(
            content_tokens("¿Cuál es la tensión máxima?"),
            vec!["cuál", "tensión", "máxima"]
        );
    }

    #[test]
    fn search_terms_fold_accents_but_content_tokens_keep_them() {
        assert_eq!(
            search_terms("¿Cuál es la tensión máxima?"),
            vec!["cual", "tension", "maxima"]
        );
        assert_eq!(
            content_tokens("¿Cuál es la tensión máxima?"),
            vec!["cuál", "tensión", "máxima"]
        );
        assert_eq!(
            search_terms("horario de atencion"),
            search_terms("horario de atención")
        );
        // Marks of other scripts are part of the word and stay.
        assert_eq!(search_terms("\u{915}\u{93e}"), vec!["\u{915}\u{93e}"]);
        assert_eq!(search_fold("ñandú"), "nandu");
        assert_eq!(search_fold("nin\u{303}o"), "nino");
    }

    fn terms(s: &str) -> Vec<String> {
        search_terms(s)
    }

    #[test]
    fn case_and_compatibility_folding() {
        assert_eq!(search_fold("Straße"), "strasse");
        assert_eq!(search_fold("STRASSE"), "strasse");
        assert_eq!(search_fold("ẞ"), "ss");
        // Greek: final sigma, tonos
        assert_eq!(search_fold("ΟΔΟΣ"), search_fold("οδοσ"));
        assert_eq!(search_fold("Ελληνικά"), "ελληνικα");
        // Turkish dotted and dotless i
        assert_eq!(search_fold("İstanbul"), "istanbul");
        assert_eq!(search_fold("ISPARTA ısparta"), "isparta isparta");
        // full-width and ligatures
        assert_eq!(search_fold("ＨＯＲＡＲＩＯ １２３"), "horario 123");
        assert_eq!(search_fold("ﬁnal"), "final");
        // Polish, Nordic, Vietnamese letters without decomposition
        assert_eq!(search_fold("Łódź"), "lodz");
        assert_eq!(search_fold("Ærø Đà Nẵng"), "aero da nang");
    }

    #[test]
    fn nfc_and_nfd_are_one_text() {
        for (a, b) in [
            ("café", "cafe\u{301}"),
            ("Ñandú", "N\u{303}andu\u{301}"),
            ("Å", "A\u{30A}"),
            (
                "한국어",
                "\u{1112}\u{1161}\u{11AB}\u{1100}\u{116E}\u{11A8}\u{110B}\u{1165}",
            ),
        ] {
            assert_eq!(terms(a), terms(b), "{a:?} vs {b:?}");
        }
        assert_eq!(terms("café"), vec!["cafe"]);
    }

    #[test]
    fn scripts_other_than_latin_are_terms() {
        assert_eq!(terms("Привет, Мир!"), vec!["привет", "мир"]);
        assert_eq!(terms("Москва"), terms("МОСКВА"));
        // Cyrillic: stress and ё go, й stays a letter of its own
        assert_eq!(terms("ёлка"), terms("елка"));
        assert_ne!(terms("мой"), terms("мои"));
        assert_eq!(terms("Καλημέρα κόσμε"), vec!["καλημερα", "κοσμε"]);
        assert_eq!(terms("مَرْحَبًا بالعالم"), vec!["مرحبا", "بالعالم"]);
        assert_eq!(terms("أهلا"), terms("اهلا"));
        assert_eq!(terms("שָׁלוֹם עולם"), vec!["שלומ", "עולמ"]);
        assert_eq!(terms("नमस्ते दुनिया"), vec!["नमस्ते", "दुनिया"]);
    }

    #[test]
    fn unspaced_scripts_give_units_and_pairs() {
        assert_eq!(terms("拉巴斯"), vec!["拉", "拉巴", "巴", "巴斯", "斯"]);
        assert_eq!(terms("茶"), vec!["茶"]);
        assert_eq!(
            terms("東京タワー"),
            vec!["東", "東京", "京", "京タ", "タ", "タワ", "ワ", "ワー", "ー"]
        );
        assert_eq!(terms("iPhone手机"), vec!["iphone", "手", "手机", "机"]);
        // Thai marks stay with their letter
        assert_eq!(terms("ที่"), vec!["ที่"]);
        assert_eq!(terms("ภาษาไทย").len(), 13);
        // a query is found inside a longer text
        let text: HashSet<String> = terms("我们的咖啡馆在拉巴斯").into_iter().collect();
        assert!(terms("咖啡").iter().all(|t| text.contains(t)));
        assert!(!terms("咖茶").iter().all(|t| text.contains(t)));
    }

    #[test]
    fn symbols_and_lone_marks_are_not_terms_and_nothing_panics() {
        assert!(terms("🍕🎉👨\u{200D}👩\u{200D}👧").is_empty());
        assert_eq!(terms("pizza 🍕 y café ☕"), vec!["pizza", "cafe"]);
        assert_eq!(terms("1\u{FE0F}\u{20E3}"), vec!["1"]);
        assert!(terms("\u{301}\u{301}\u{93E}").is_empty());
        assert!(terms("\u{0}\u{FFFF}\u{FFFD}\u{10FFFF}\u{E000}").is_empty());
        assert_eq!(terms("می\u{200C}خواهم"), terms("میخواهم"));
        // a very long run, a million combining marks, a long unspaced run
        let long = "a".repeat(200_000);
        assert_eq!(terms(&long).len(), 1);
        let zalgo = format!("e{}", "\u{301}\u{323}".repeat(500_000));
        assert_eq!(terms(&zalgo), vec!["e"]);
        let han = "漢".repeat(100_000);
        assert_eq!(terms(&han).len(), 2 * 100_000 - 1);
        let mixed: String = (0u32..0x30000).filter_map(char::from_u32).collect();
        let _ = terms(&mixed);
        let _ = terms(&mixed.chars().rev().collect::<String>());
    }

    #[test]
    fn token_set_ignores_order_and_stopwords() {
        assert_eq!(
            token_set_key("what is the reference voltage of the lm317"),
            token_set_key("LM317 reference voltage?")
        );
    }

    fn conformance_file(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("conformance")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// The stopword list is part of the standard: conformance/stopwords.txt
    /// is the text, and the constant above has to be the same set.
    #[test]
    fn the_stopword_list_is_the_one_in_the_standard() {
        let file = conformance_file("stopwords.txt");
        let in_file: HashSet<&str> = file.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(&in_file, stopwords());
    }

    /// conformance/normalization.json: the lines every implementation must reproduce.
    #[test]
    fn normalization_vectors() {
        let v: serde_json::Value =
            serde_json::from_str(&conformance_file("normalization.json")).unwrap();
        let strings = |x: &serde_json::Value| -> Vec<String> {
            x.as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        };
        for c in v["normalize"].as_array().unwrap() {
            let input = c["input"].as_str().unwrap();
            assert_eq!(
                canonical(input),
                c["canonical"].as_str().unwrap(),
                "canonical({input:?})"
            );
            assert_eq!(
                content_tokens(input),
                strings(&c["tokens"]),
                "content_tokens({input:?})"
            );
            assert_eq!(
                token_set_key(input),
                strings(&c["key"]),
                "token_set_key({input:?})"
            );
        }
        for pair in v["same_key"].as_array().unwrap() {
            let (a, b) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
            assert_eq!(
                token_set_key(a),
                token_set_key(b),
                "{a:?} and {b:?} must match"
            );
        }
        for pair in v["different_key"].as_array().unwrap() {
            let (a, b) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
            assert_ne!(
                token_set_key(a),
                token_set_key(b),
                "{a:?} and {b:?} must not match"
            );
        }
        for c in v["search_terms"].as_array().unwrap() {
            let input = c["input"].as_str().unwrap();
            assert_eq!(
                search_terms(input),
                strings(&c["terms"]),
                "search_terms({input:?})"
            );
        }
        for pair in v["same_terms"].as_array().unwrap() {
            let (a, b) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
            assert_eq!(
                search_terms(a),
                search_terms(b),
                "{a:?} and {b:?} must have the same search terms"
            );
        }
        for pair in v["different_terms"].as_array().unwrap() {
            let (a, b) = (pair[0].as_str().unwrap(), pair[1].as_str().unwrap());
            assert_ne!(search_terms(a), search_terms(b), "{a:?} and {b:?} must not");
        }
        for c in v["search_fold"].as_array().unwrap() {
            let input = c["input"].as_str().unwrap();
            assert_eq!(
                search_fold(input),
                c["folded"].as_str().unwrap(),
                "search_fold({input:?})"
            );
        }
        for c in v["matches"].as_array().unwrap() {
            let (query, text) = (c["query"].as_str().unwrap(), c["text"].as_str().unwrap());
            let q = search_terms(query);
            let t: HashSet<String> = search_terms(text).into_iter().collect();
            let got = !q.is_empty() && q.iter().all(|x| t.contains(x));
            assert_eq!(got, c["match"].as_bool().unwrap(), "{query:?} in {text:?}");
        }
        for c in v["ids"].as_array().unwrap() {
            assert_eq!(
                stable_id(c["input"].as_str().unwrap()),
                c["id"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn stable_id_is_deterministic() {
        assert_eq!(stable_id("fact:x"), stable_id("fact:x"));
        assert_ne!(stable_id("fact:x"), stable_id("fact:y"));
        assert_eq!(stable_id("fact:x").len(), 16);
    }

    #[test]
    fn today_looks_like_a_date() {
        let t = today();
        assert_eq!(t.len(), 10);
        assert_eq!(&t[4..5], "-");
    }

    #[test]
    fn now_is_an_iso_timestamp_that_starts_with_today() {
        let n = now();
        assert_eq!(n.len(), 20);
        assert!(n.ends_with('Z') && &n[10..11] == "T" && &n[13..14] == ":");
        assert!(n.starts_with(&today()) || n[..10] > *"2026-01-01");
    }

    #[test]
    fn wikilinks_are_obsidian_links_and_code_is_not() {
        let text = "Ver [[Reiniciar el Router]] y [[IP de administración|la IP]].
                    Otra vez [[reiniciar el router]], sin repetir.
                    En código `[[no es enlace]]` ni esto:
                    ```
[[tampoco]]
```
                    [[]] vacío, [[sin cerrar";
        assert_eq!(
            wikilinks(text),
            vec![
                "reiniciar el router".to_string(),
                "ip de administración".to_string()
            ]
        );
    }
}
