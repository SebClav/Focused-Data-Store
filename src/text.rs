//! Text normalisation shared by ID generation and retrieval.

use unicode_normalization::UnicodeNormalization;

/// Words ignored by retrieval, one per line.
const STOPWORDS: &str = include_str!("../stopwords.txt");

/// Case- and whitespace-insensitive form used to derive context IDs.
pub fn fold(s: &str) -> String {
    s.nfc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Normalised question text: NFC, lowercase, punctuation replaced by spaces.
pub fn canonical(s: &str) -> String {
    s.nfc()
        .collect::<String>()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() || is_mark(c) {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Order-independent set of search terms, for matching reworded questions.
pub fn token_key(s: &str) -> Vec<String> {
    let mut terms = search_terms(s);
    terms.sort();
    terms.dedup();
    terms
}

/// Folded, stopword-free terms of a text.
pub fn search_terms(s: &str) -> Vec<String> {
    search_fold(s)
        .split(|c: char| !c.is_alphanumeric() && !is_mark(c))
        .filter(|t| !t.is_empty() && !STOPWORDS.lines().any(|word| word == *t))
        .map(str::to_string)
        .collect()
}

/// Combining marks (accents and similar) across the Unicode blocks that hold them.
fn is_mark(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036f | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x20d0..=0x20ff | 0xfe20..=0xfe2f
    )
}

/// Lowercases, expands compatibility forms and strips accents.
fn search_fold(s: &str) -> String {
    let compatibility: String = s.nfkc().flat_map(char::to_lowercase).collect();
    let compatibility = compatibility
        .replace('ß', "ss")
        .replace('ς', "σ")
        .replace('ı', "i")
        .replace('ł', "l")
        .replace('ø', "o")
        .replace(['đ', 'ð'], "d")
        .replace('ħ', "h")
        .replace('ŧ', "t")
        .replace('æ', "ae")
        .replace('œ', "oe")
        .replace('þ', "th");
    compatibility
        .nfd()
        .filter(|c| !is_mark(*c))
        .collect::<String>()
        .nfc()
        .collect()
}
