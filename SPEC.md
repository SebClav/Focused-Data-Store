# FDS file format specification

This document describes the on-disk `.fds` database format implemented by FDS. Version 4 is the current writable format. Readers also support legacy versions 2 and 3; new writes always use version 4.

## 1. Integer and text conventions

- Header and index integers are unsigned, little-endian values of the width shown below.
- Text is UTF-8.
- A file is a byte sequence. A reader must validate bounds before slicing any region.
- Version 4 uses CBOR as defined by [RFC 8949](https://www.rfc-editor.org/rfc/rfc8949). Encoders order map entries by the bytewise order of their encoded keys for deterministic output.

## 2. Version 4 file layout

The file consists of a 64-byte header, metadata, zero or more records, and an index. The metadata and index have checksums in the header; each record has its own checksum in the index.

| Offset | Size | Field | Required value or meaning |
|---:|---:|---|---|
| 0 | 8 | Magic | `89 46 44 53 0D 0A 1A 0A` (`\x89FDS\r\n\x1a\n`) |
| 8 | 2 | Format major | `4` |
| 10 | 2 | Format minor | `0` for this writer |
| 12 | 4 | Application ID | `0` when unused |
| 16 | 4 | User version | `0` when unused |
| 20 | 4 | Record count | Number of index entries |
| 24 | 8 | Metadata offset | Byte offset of the metadata CBOR map |
| 32 | 4 | Metadata length | Number of metadata bytes |
| 36 | 8 | Index offset | Byte offset of the index |
| 44 | 4 | Metadata CRC-32 | CRC-32 of the metadata bytes |
| 48 | 4 | Index CRC-32 | CRC-32 of the complete index bytes |
| 52 | 8 | Reserved | Writers set all bytes to zero; readers ignore them |
| 60 | 4 | Header CRC-32 | CRC-32 of bytes `0..60` |

Metadata begins at offset 64. Records follow metadata consecutively. The index follows the record region and contains one 16-byte entry per record:

| Entry offset | Size | Field |
|---:|---:|---|
| 0 | 8 | Record byte offset |
| 8 | 4 | Record byte length |
| 12 | 4 | CRC-32 of that record's encoded bytes |

Offsets are absolute from the start of the file. A reader must reject regions that exceed the file, metadata/index overlap, records outside the record region, and checksum mismatches.

### 2.1 CRC-32

Checksums use CRC-32/ISO-HDLC: reflected polynomial `0xEDB88320`, initial value `0xFFFFFFFF`, and final XOR `0xFFFFFFFF`. The standard check value for ASCII `123456789` is `0xCBF43926`. Checksum integers are little-endian.

### 2.2 Metadata map

Metadata is a CBOR map with these text keys:

| Key | CBOR type | Meaning |
|---|---|---|
| `name` | text | Database name |
| `created` | text | Creation date, conventionally `YYYY-MM-DD` |
| `updated` | text | Last update date, conventionally `YYYY-MM-DD` |
| `embedding_model` | text | Embedding model identifier, empty when unused |
| `embedding_dims` | unsigned integer | Embedding vector dimensions, zero when unused |

Readers default absent metadata keys to empty strings or zero and skip unknown keys.

### 2.3 Record map

Each record is a CBOR map. Required keys are:

| Key | CBOR type | Meaning |
|---|---|---|
| `id` | text | Stable record identifier |
| `kind` | text | `fact`, `procedure`, or `knowledge` |
| `collection` | text | Logical collection name |
| `body` | text | Answer, procedure payload, or knowledge text |
| `title` | text | Human-readable title or retrieval label |
| `source` | text | Provenance identifier or source description |
| `verified_by` | text | Provenance/review label such as `human`, `ingest`, or `model:<name>` |
| `last_verified` | text | Last verification date, conventionally `YYYY-MM-DD` |
| `tokens` | unsigned integer | Stored token-count estimate |

Optional keys are:

| Key | CBOR type | Meaning |
|---|---|---|
| `question` | text or null | Canonical question for a fact, or intent for a procedure |
| `embedding` | byte string | 32-bit IEEE-754 floats encoded little-endian |
| `type` | text | Owner-defined type; `context` has defined query behavior |
| `attrs` | map of text to text | Owner-defined metadata |
| `links` | array of text | Explicit link targets; targets need not exist when written |

Absent optional fields decode as null or empty values. Readers skip unknown record keys. Duplicate known keys, unknown `kind` values, malformed values, or trailing bytes in a CBOR object are errors.

## 3. Atomic writes and compatibility

Writers construct a complete new database, write it to a unique temporary file in the same directory, flush it, then replace the destination. A writer must refuse to rewrite a version 4 file whose minor version is newer than its supported minor version; doing so could discard fields it does not understand. Readers may accept newer minor versions of major version 4 and skip unknown fields.

Versions 2 and 3 are read using frozen positional `bincode` schemas. Version 2 has records without `type`, `attrs`, or `links`. Version 3 adds those fields. These legacy schemas are retained for migration and compatibility; independent implementations should target version 4 rather than depend on Rust's `bincode` representation.

### 3.1 Legacy version 2 and 3 layout

The legacy header is 32 bytes:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | Magic: ASCII `FDS2` or `FDS3` |
| 4 | 4 | Version integer: `2` or `3` |
| 8 | 4 | Record count |
| 12 | 8 | Metadata offset |
| 20 | 4 | Metadata length |
| 24 | 8 | Index offset |

The header is followed by bincode metadata, bincode records, then `record_count` index entries of `(offset u64, length u32)`, all little-endian. A version 2 record contains, in order: `id`, `kind`, `collection`, `question`, `body`, `title`, `source`, `verified_by`, `last_verified`, `tokens`, and `embedding`. Version 3 appends `type`, `attrs`, and `links`. Metadata fields, in order, are `name`, `created`, `updated`, `embedding_model`, and `embedding_dims`.

## 4. Query and trust semantics

`kind` describes how a record is retrieved, not whether it is true. `verified_by` and `source` carry provenance labels, but the file format does not authenticate them. Applications must not treat a lexical match or a label such as `exact`, `strong`, or `weak` as proof of truth. Human review and host filesystem permissions remain outside the file format.

The CLI records explicit reviews in `attrs`: `fds.review.status` is `approved` or `pending`, `fds.review.reviewer` is the self-declared reviewer name, and `fds.review.note` is an optional note. An approved record also has `verified_by = human` and a non-empty `last_verified` date. Records without these review fields, including legacy records labelled `human`, are treated as pending by `--review-status` and `review list`. Creating or replacing a record resets its review status to pending and clears the reviewer and verification date.

These fields are mutable metadata, not authentication or an append-only history. A host must restrict write access if agents or other processes must not approve or alter records. The format does not prove that the named reviewer is a human or that a review actually occurred.
