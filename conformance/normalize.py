"""Normalisation and identifiers (SPEC.md §3.1 and §4.1), second implementation.

Written from the spec, not from the Rust code. Standard library only.
"""

import os
import unicodedata

HERE = os.path.dirname(os.path.abspath(__file__))

with open(os.path.join(HERE, "stopwords.txt"), encoding="utf-8") as _f:
    STOPWORDS = frozenset(line.rstrip("\n") for line in _f if line.strip())


def _fold(text):
    return unicodedata.normalize("NFC", text.lower())


def _is_alnum(c):
    return unicodedata.category(c)[0] in ("L", "N", "M")


def canonical(text):
    """Lowercase, then NFC; every character that is not a letter, a number, a
    combining mark or white space becomes a space; runs of white space
    collapse to one; trimmed."""
    lowered = _fold(text)
    spaced = "".join(c if (_is_alnum(c) or c.isspace()) else " " for c in lowered)
    return " ".join(spaced.split())


def content_tokens(text):
    """Maximal runs of letters, numbers and combining marks of the folded text
    (lower case, then NFC), in order, without the stopwords."""
    lowered = _fold(text)
    tokens, run = [], []
    for c in lowered + " ":
        if _is_alnum(c):
            run.append(c)
        elif run:
            tokens.append("".join(run))
            run = []
    return [t for t in tokens if t not in STOPWORDS]


def token_set_key(text):
    return sorted(set(content_tokens(text)))


# --- search fold and search terms (SPEC.md 4.1) -----------------------------

_INVISIBLE = frozenset(
    [0x00AD, 0x200C, 0x200D, 0x200E, 0x200F, 0x2060, 0x2061, 0x2062, 0x2063, 0x2064, 0xFEFF]
    + list(range(0x202A, 0x202F))
    + list(range(0x2066, 0x206A))
)

# Letters with no decomposition that the search fold replaces (after lower case).
_LETTERS = {
    "ß": "ss", "ς": "σ", "ı": "i", "ł": "l", "ø": "o", "đ": "d", "ð": "d", "ħ": "h",
    "ŧ": "t", "æ": "ae", "œ": "oe", "þ": "th",
    "ך": "כ", "ם": "מ", "ן": "נ", "ף": "פ", "ץ": "צ",
}

# Removable combining marks outside U+0300..U+036F (those have their own rule).
_MARK_RANGES = (
    (0x0591, 0x05BD), (0x05BF, 0x05BF), (0x05C1, 0x05C2), (0x05C4, 0x05C5), (0x05C7, 0x05C7),
    (0x0610, 0x061A), (0x0640, 0x0640), (0x064B, 0x065F), (0x0670, 0x0670),
    (0x06D6, 0x06DC), (0x06DF, 0x06E4), (0x06E7, 0x06E8), (0x06EA, 0x06ED),
    (0x1AB0, 0x1AFF), (0x1DC0, 0x1DFF), (0x20D0, 0x20FF), (0xFE00, 0xFE0F),
    (0xFE20, 0xFE2F), (0xE0100, 0xE01EF),
)


def _removable(c, base):
    o = ord(c)
    if 0x0300 <= o <= 0x036F:
        if 0x0400 <= ord(base) <= 0x052F:
            # On a Cyrillic letter only the stress marks and the diaeresis of "е" go.
            return o in (0x0300, 0x0301) or (base == "е" and o == 0x0308)
        return True
    for lo, hi in _MARK_RANGES:
        if lo <= o <= hi:
            return True
    return False


def search_fold(text):
    """NFKC; lower case; the letter replacements; invisible characters deleted;
    NFD; removable marks dropped; NFC. Stored text is never changed."""
    if text.isascii():
        return text.lower()
    pre = []
    for c in unicodedata.normalize("NFKC", text):
        if ord(c) in _INVISIBLE:
            continue
        for l in c.lower():
            pre.append(_LETTERS.get(l, l))
    out, base = [], "\0"
    for c in unicodedata.normalize("NFD", "".join(pre)):
        if _removable(c, base):
            continue
        if unicodedata.combining(c) == 0:
            base = c
        out.append(c)
    return unicodedata.normalize("NFC", "".join(out))


_FOLDED_STOPWORDS = frozenset(search_fold(w) for w in STOPWORDS)

# Scripts written without spaces between words: runs of these are cut into units.
_UNSPACED = (
    (0x0E00, 0x0EFF), (0x1000, 0x109F), (0x1780, 0x17FF), (0x2E80, 0x2FDF),
    (0x3000, 0x312F), (0x31A0, 0x31BF), (0x31F0, 0x31FF), (0x3400, 0x4DBF),
    (0x4E00, 0x9FFF), (0xAC00, 0xD7AF), (0xF900, 0xFAFF), (0x20000, 0x3134F),
)


def _unspaced(c):
    o = ord(c)
    for lo, hi in _UNSPACED:
        if lo <= o <= hi:
            return True
    return False


def _run_terms(run, out):
    if not any(_unspaced(c) for c in run):
        out.append(run)
        return
    word, units, in_unspaced = [], [], False

    def flush():
        for i, u in enumerate(units):
            out.append(u)
            if i + 1 < len(units):
                out.append(u + units[i + 1])
        units.clear()

    for c in run:
        mark = unicodedata.category(c)[0] == "M"
        unspaced = in_unspaced if mark else _unspaced(c)  # a mark takes the class before it
        if unspaced:
            if word:
                out.append("".join(word))
                word.clear()
            if mark and units:
                units[-1] += c
            else:
                units.append(c)
        else:
            flush()
            word.append(c)
        in_unspaced = unspaced
    flush()
    if word:
        out.append("".join(word))


def search_terms(text):
    """The terms stage 2 matches on (SPEC.md 4.1, search terms): runs of letters,
    numbers and marks of the search fold, a run starting at a letter or a number;
    runs of unspaced scripts cut into units and pairs; without the stopwords."""
    folded = search_fold(text)
    out, run = [], []
    for c in folded + " ":
        cat = unicodedata.category(c)[0]
        if cat in ("L", "N") or (run and cat == "M"):
            run.append(c)
        elif run:
            _run_terms("".join(run), out)
            run = []
    return [t for t in out if t not in _FOLDED_STOPWORDS]


def stable_id(text):
    """FNV-1a, 64 bits, over the UTF-8 bytes; 16 lower-case hex digits."""
    h = 0xCBF29CE484222325
    for b in text.encode("utf-8"):
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"
