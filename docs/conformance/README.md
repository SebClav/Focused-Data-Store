# FDS conformance fixtures

Reference fixtures for the FDS file format, normalisation, policies and audit chains. The Rust tests consume these files; Python is not required.

## Run it

```
cargo test -p fds --test conformance   # the Rust one, on the same files
cargo test -p fds-cli                   # normalisation, policy, redaction and audit vectors
```

## What is in it

| Path | What it is |
|---|---|
| `cases/valid/` | Files that must be read. Each `name.fds` has a `name.json` |
| `cases/invalid/` | Files that must be refused, each with the reason |
| `cases/MANIFEST.json` | The list of cases, so a runner does not scan directories |
| `normalization.json` | Normalisation, search fold, search terms and identifier vectors (SPEC §3.1, §4.1): accents, ß, İ, final sigma, NFC/NFD, full-width, Cyrillic, Greek, Arabic, Hebrew, Indic, Thai, CJK, emoji |
| `policy.json` | The guard's deterministic parts (SPEC §11): checks, template fills, redactions |
| `cases/audit/` | Audit chains, intact and broken, with where each breaks (SPEC §11.4) |
| `stopwords.txt` | The stopword list, part of the standard (SPEC §4.1) |

## A valid case

```json
{
  "description": "what the case checks",
  "minor": 0,
  "writable": true,
  "reproducible": true,
  "meta": { "name": "...", "created": "...", "updated": "...", "embedding_model": "", "embedding_dims": 0 },
  "records": [ { "id": "...", "kind": "fact", "question": null, "type": "", "attrs": {}, "links": [], "...": "..." } ]
}
```

- Reading `name.fds` must give exactly `meta` and `records`. A missing optional
  field is `null` (`question`, `embedding`), `""` (`type`), `{}` or `[]`.
- `minor` is the file's `format_minor`.
- `reproducible`: writing `meta` and `records` back must give the file, byte for
  byte (SPEC §6.4, W1 to W3). Files that carry encodings a reader accepts and a
  writer does not produce (`lenient_encoding`, `future_minor`) say `false`.
- `writable`: `false` for a file from a newer minor, which a writer must refuse
  to rewrite (SPEC §6.2, C3).

## A damaged case

```json
{
  "description": "what is wrong with the file",
  "error": "checksum",
  "stage": "record",
  "readable": [0, 2]
}
```

- `error` is one of the five categories of SPEC §6.6: `not_fds`, `truncated`,
  `unsupported_major`, `checksum`, `malformed`. The order of the checks in that
  section decides which one a file with two faults gets.
- `stage` is `open` when the file cannot be opened at all, and `record` when it
  opens and reading some record fails.
- `readable` (only with `record`) lists the records that must still be read
  one by one: a damaged record does not take the others with it.

Besides the files, a good file cut at every length must be `truncated`, and
the Rust test checks that.

## Testing another implementation

For every entry in `cases/MANIFEST.json`, read the `.fds` file and compare it
with the expected `.json`. SPEC §13 defines the conformance levels.

## Changing a case

Update fixtures only for a documented specification change or a new test case.
Review binary and expected JSON changes together and run the Rust suite.
