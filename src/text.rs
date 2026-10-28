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

/// Words that reverse a statement. Dropping them would make a question and
/// its opposite look identical, so they are never treated as stopwords.
const NEGATIONS: &[&str] = &["no", "nor", "not", "sin"];

/// Words that change what is being asked (can, should, ...). Also kept when
/// deciding whether two questions are the same one.
const MODALS: &[&str] = &["can", "could", "should", "will", "would"];

/// Order-independent set of terms, for deciding that two questions are the
/// same question reworded. Stricter than search: negations and modals count.
pub fn token_key(s: &str) -> Vec<String> {
    let mut terms = terms(s, |t| NEGATIONS.contains(&t) || MODALS.contains(&t));
    terms.sort();
    terms.dedup();
    terms
}

/// Folded, stopword-free terms of a text. Negations are kept.
pub fn search_terms(s: &str) -> Vec<String> {
    terms(s, |t| NEGATIONS.contains(&t))
}

fn terms(s: &str, keep: impl Fn(&str) -> bool) -> Vec<String> {
    expand_contractions(&search_fold(s))
        .split(|c: char| !c.is_alphanumeric() && !is_mark(c))
        .filter(|t| !t.is_empty() && (keep(t) || !STOPWORDS.lines().any(|word| word == *t)))
        .map(str::to_string)
        .collect()
}

/// Spells out English negative contractions so "can't", "cannot" and
/// "can not" give the same terms.
fn expand_contractions(s: &str) -> String {
    s.replace('’', "'")
        .replace("can't", "can not")
        .replace("cannot", "can not")
        .replace("won't", "will not")
        .replace("shan't", "shall not")
        .replace("n't", " not")
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

#[cfg(test)]
mod tests {
    use super::{search_terms, token_key};

    #[test]
    fn a_question_and_its_negation_do_not_share_a_key() {
        let plain = token_key("Can users delete records?");
        for negated in [
            "Cannot users delete records?",
            "Can not users delete records",
            "Users can not delete records",
            "Can't users delete records?",
            "Can’t users delete records?",
            "Do users not delete records?",
        ] {
            assert_ne!(plain, token_key(negated), "{negated}");
        }
    }

    #[test]
    fn spellings_of_the_same_negation_share_a_key() {
        let key = token_key("Cannot users delete records?");
        assert_eq!(key, token_key("Can not users delete records"));
        assert_eq!(key, token_key("Can't users delete records"));
        assert_eq!(key, token_key("records users delete cannot"));
        assert_eq!(token_key("Don't ship it"), token_key("do not ship it"));
        assert_eq!(token_key("Won't it work?"), token_key("will not it work"));
    }

    #[test]
    fn questions_that_differ_by_a_modal_do_not_share_a_key() {
        assert_ne!(
            token_key("Can users delete records?"),
            token_key("Should users delete records?")
        );
        assert_ne!(
            token_key("Can users delete records?"),
            token_key("Users delete records")
        );
    }

    #[test]
    fn rewording_with_plain_stopwords_still_shares_a_key() {
        assert_eq!(
            token_key("What is the service port?"),
            token_key("service port")
        );
        assert_eq!(
            token_key("What is the service port?"),
            token_key("port service what is the")
        );
    }

    #[test]
    fn search_keeps_negations_but_not_other_stopwords() {
        assert_eq!(
            search_terms("users do not delete the records"),
            ["users", "not", "delete", "records"]
        );
    }
}
