# FDS Specification v1 — Focused Model Data Store

**Status:** draft, experimental · **Date:** 2026-10-29 · **File format:** FDS 4.0

**Revision 2026-10-29:** negations are no longer stopwords (§4.1), so the list has
193 words, not 198. No file is affected; a token set key computed with the old
list differs only for texts that contain `no`, `not`, `nor`, `cannot` or `sin`.

FDS is a database for knowledge that AI systems consume. It stores three kinds
of records — verified facts, procedures, and knowledge — in a single portable
file, and answers queries with records, a confidence level, and provenance.

This document is normative. "MUST", "SHOULD" and "MAY" are used as in RFC 2119.
The reference implementation is the `fds` command-line tool in this repository;
`conformance/` holds a second implementation of the file format and the test
files both must pass (§13).

---

## 1. What FDS is, and what it is not

A FDS database **stores and queries**. It **does not generate**.

A query returns records, the way SQL returns rows. What happens next — feeding
the records to a small local model, a large hosted model, a script, or a
person — is the caller's decision. This separation is the point:

- A **RAG pipeline** retrieves text *and generates an answer*. Retrieval is an
  internal step of one application.
- A **FDS database** is the retrieval side as a standalone, portable artifact
  with its own data model. Any number of applications and models can use the
  same database.

FDS also **does not prohibit by itself**. A query reports how confident a match
is (§5) and never instructs the caller to refuse, hedge, or answer. What FDS can
do is keep the rules a person wrote and answer, by comparison and with no model,
whether an action breaks one (§11.2). Acting on that answer is the job of a
separate process, the guard (§11.3), which holds the credentials and refuses.

FDS may use a model **when a record is written** — to compute its vector
(§4.3) or to describe an image (§3.4) — the way a compiler runs before a
program does. It **never uses a model to answer a query**. And a record always
says what produced it: text a model wrote carries `verified_by: model:<name>`
until a person checks it (§3).

FDS is **moldable without retraining**. There is nothing to retrain: changing
what a system knows is editing a record, and new kinds of content are a new
`type`, not new code (§3.3).

---

## 2. Record kinds

Every record has exactly one kind.

| Kind | Holds | Matched by | Needs a model? |
|---|---|---|---|
| `fact` | a canonical question and its verified answer | the question | no |
| `procedure` | a canonical intent and its steps (e.g. fields, selectors, actions) | the intent | no |
| `knowledge` | a fragment of a source document | full-text and optional vector search | no (vectors optional) |

Facts and procedures are **compiled** knowledge: someone decided the answer or
the steps once, so nobody needs to rediscover them. Knowledge is raw material.

A kind says **how a record is answered**, and the list is closed. What a record
**is** — a command, an image, a context, a recipe — is its `type` (§3.3), and
that list is open.

## 3. Record fields

| Field | Type | Required | Meaning |
|---|---|---|---|
| `id` | string | yes | Stable identifier, see §3.1 |
| `kind` | `fact` \| `procedure` \| `knowledge` | yes | |
| `collection` | string | yes | Namespace; `default` if not given |
| `question` | string | facts, procedures | Canonical question or intent |
| `body` | string | yes | Answer (fact), steps as JSON (procedure), text (knowledge) |
| `title` | string | yes | Heading path or short label |
| `source` | string | yes | Where the content came from (path, URL, `manual`) |
| `verified_by` | string | yes | `human`, `ingest`, or `model:<name>` |
| `last_verified` | date `YYYY-MM-DD` | yes | When it was last confirmed true |
| `tokens` | integer | yes | Whitespace word count of `body` |
| `embedding` | float array | no | Present only after `fds embed` |
| `type` | string | yes, may be empty | What the record is, in the owner's words (§3.3) |
| `attrs` | map of string to string | yes, may be empty | Free metadata (§3.3) |
| `links` | string array | yes, may be empty | Explicit link targets, beyond `[[links]]` in `body` (§4.4) |

### 3.1 Identifiers

`id` MUST be deterministic, so that re-ingesting the same content yields the
same identifier:

- fact / procedure: hash of `kind` + canonical `question`
- knowledge: hash of `source` + fragment position
- context: hash of `context` + canonical name (§4.5)
- image: hash of `image` + source name (§3.4)

The reference implementation hashes these strings with 64-bit FNV-1a (offset
basis `0xcbf29ce484222325`, prime `0x100000001b3`, over the UTF-8 bytes) and
writes the result as 16 lower-case hex digits:

| Record | String hashed |
|---|---|
| fact | `fact:` + canonical question |
| procedure | `procedure:` + canonical intent |
| knowledge | `knowledge:` + source + `#` + fragment position |
| context | `context:` + canonical name |
| image | `image:` + source name |

`conformance/normalization.json` lists hashes to reproduce. Tools SHOULD accept
any unambiguous prefix of at least 6 characters.

### 3.2 Procedure body

A procedure `body` MUST be a JSON object with a `steps` array. Each step SHOULD
have an `action` and MAY carry any further keys the executor understands:

```json
{
  "steps": [
    { "action": "open",  "url": "/clientes/nuevo" },
    { "action": "fill",  "selector": "#cli_nombre", "field": "nombre" },
    { "action": "click", "selector": "button[data-qa='guardar']" }
  ]
}
```

FDS stores and returns procedures. It never executes them.

### 3.3 Types and attributes

`type` is free text. An owner can call records `command`, `receta`,
`manual-page` or anything else, and nothing in FDS has to change: listing and
filtering work on any value (§7, §8.2). FDS gives meaning to exactly two:

- `context` — a record that bundles instructions and other records (§4.5).
  Contexts are loaded by name and are never returned as the answer to a query.
- `image` — knowledge written from an image by a vision model (§3.4).

`attrs` is a map of names to string values for whatever a type needs: a file's
hash, a shell, a vendor. It is stored sorted, so two equal records serialise to
the same bytes.

### 3.4 Images

FDS **does not store images**. It stores what a vision model reads in one, as a
`knowledge` record of `type: image` that queries find like any other text.

- The model is asked to transcribe first — every piece of visible text,
  verbatim and untranslated, one per line under `TEXT` — and only then to
  describe what the image shows under `DESCRIPTION`, optionally in a given
  language. Asked for a single description, a small model summarises and drops
  exactly what someone will search for later, such as an error code.
- `verified_by` is `model:<name>`. `attrs` carries `file`, `sha256`, `bytes`,
  `media_type` and `described_by`.
- An image is described when it is added, never when it is queried. Adding the
  same bytes under the same name again is `unchanged` and is **not** described
  again — which also keeps a description a person has corrected.
- A preview of an import never calls the model (§8.3).

The reference implementation uses `qwen2.5vl:3b` through Ollama; any vision
model can be named instead.

### 3.5 The change journal

Developers now read code that agents wrote and that they never saw written. The
journal is how an agent leaves, next to the work, an account a person can read:
what changed, why, where. It is not a new kind of record or a new file: an
entry is an ordinary `knowledge` record of `type: change`, in `collection:
journal`, so it is stored, queried, verified and exported like any other.

| Field | Holds |
|---|---|
| `title` | one line, 200 characters at most: what changed |
| `body` | why, how, and what a reader should watch out for; if empty, the title |
| `attrs.at` | when, UTC, `YYYY-MM-DDTHH:MM:SSZ` |
| `attrs.agent` | the agent's name; absent when a person wrote it |
| `attrs.branch`, `attrs.commit` | where in git the change is, when known |
| `attrs.role` | the part this step plays, in the owner's words: `me`, `reasoning`, `decision`, `execution`… |
| `attrs.status` | where it stands: `running`, `finished`, `blocked`… |
| `attrs.to` | who it was handed to, when it was a delegation |
| `attrs.seq` | its position in the database, counted as entries are written |
| `attrs.files` | the paths it touched, one per line, without repeats |
| `verified_by` | `model:<agent>` until a person reviews it, then `human` |
| `source` | `agent:<name>`, or `cli` |

The `id` comes from the agent, the time and the title (§3.1), so the same
entry written twice in a second is one entry and a later one is another:
a journal grows, it does not overwrite.

An agent writes an entry with `fds_log` (§9); a person, with `fds log`.
`fds journal show` lists entries newest first (`--timeline` lists them as a numbered
black box, oldest first: `#04 [reasoning] technical decision delegated → decision
agent`, `#06 [execution] task 1 · running`; the bracket is the role, else the agent,
else `person`), `fds journal export` writes them as
a Markdown page grouped by day, and `fds journal publish` commits that page, and
the same entries as `journal.json`, to a branch of a git repository (§7.2).
Entries are records, so `fds query` also finds them: an agent can ask what was
done about the login before changing it. Callers that want only answers filter
with `verified_by`.

---

## 4. Query semantics

A query has a text, an optional `collection`, and a token `budget`
(default 400). Evaluation is in two stages and is deterministic given the same
database. The `collection` filter applies to both stages, before any matching
or ranking.

A query MAY be restricted to records **verified by** given parties
(`verified_by`: a list; a value ending in `:` is a prefix, so `model:` means
any model). The restriction applies exactly as `collection` does: at stage 1,
at stage 2, and to linked records (§4.4) — a link never brings in a record the
query excluded. It is a capability, not a policy: FDS does not decide which
provenance to trust; it lets the caller ask for the one it does.

### 4.1 Normalisation

- **folding**: lower-case the text, then convert it to Unicode normalisation form
  NFC. A letter typed as itself (`é`) and the same letter typed as a base plus a
  combining accent (`e` and U+0301, as macOS file names and some copy-and-paste
  sources produce) are one text after folding.
- **canonical form**: fold; turn every character that is not a letter (Unicode
  category L), a number (N), a combining mark (M) or white space into a space;
  collapse each run of white space into one space; trim. Marks stay because in
  many scripts (Devanagari, Thai, Arabic with vowels) they are part of the
  word.
- **content tokens**: the maximal runs of letters, numbers and marks of the
  folded text, in order, without the stopwords.
- **token set key**: the content tokens, de-duplicated and sorted bytewise
  (UTF-8). Two texts with the same key are the same question for stage 1.
- **stopwords**: the 193 English and Spanish words of `conformance/stopwords.txt`.
  The list is part of this standard, because it decides which queries match:
  it changes only with a new version of this document. A **negation** is never
  a stopword (`no`, `not`, `nor`, `cannot`, `sin`, and any other word that
  reverses a statement): without it a question and its opposite have the same
  content tokens, and the stored answer to one would come back as `exact` for
  the other. A tool MUST NOT add a negation to the list.
- **search fold**: what makes `Straße` and `STRASSE`, `atención` and `atencion`,
  `ＣＡＦＥ` and `cafe`, and a letter written precomposed or decomposed one text for
  stage 2. In this order:
  1. convert to NFKC (compatibility forms: full-width and half-width letters and
     digits, ligatures such as `ﬁ`, Arabic presentation forms, half-width kana);
  2. delete the invisible format characters U+00AD, U+200C to U+200F, U+202A to
     U+202E, U+2060 to U+2064, U+2066 to U+2069 and U+FEFF (so a Persian word
     typed with or without its half-space is one word);
  3. lower-case every character with its Unicode full lower-case mapping, one
     character at a time, so `İ` becomes `i` and U+0307, `Σ` becomes `σ`;
  4. replace the letters that have no decomposition: `ß` → `ss`, `ς` → `σ`,
     `ı` → `i`, `ł` → `l`, `ø` → `o`, `đ` and `ð` → `d`, `ħ` → `h`, `ŧ` → `t`,
     `æ` → `ae`, `œ` → `oe`, `þ` → `th`, and the five final forms of Hebrew
     `ך ם ן ף ץ` → `כ מ נ פ צ`;
  5. convert to NFD and drop the **removable marks**: U+0300 to U+036F (but see
     below for Cyrillic), the Hebrew points and cantillation U+0591 to U+05BD,
     U+05BF, U+05C1, U+05C2, U+05C4, U+05C5, U+05C7, the Arabic tatweel U+0640 and
     vowel and recitation marks U+0610 to U+061A, U+064B to U+065F, U+0670,
     U+06D6 to U+06DC, U+06DF to U+06E4, U+06E7, U+06E8, U+06EA to U+06ED, the
     combining blocks U+1AB0 to U+1AFF, U+1DC0 to U+1DFF, U+20D0 to U+20FF,
     U+FE20 to U+FE2F, and the variation selectors U+FE00 to U+FE0F and
     U+E0100 to U+E01EF. On a Cyrillic base letter (U+0400 to U+052F) only U+0300,
     U+0301 and, on `е`, U+0308 are removable, so `ё` and `е` are one letter while
     `й`, `ї` and `ў` stay letters of their own. The marks of Devanagari and the
     other Indic scripts, Thai, Lao, Khmer, Myanmar and the Japanese voiced marks
     are part of the word and stay;
  6. convert to NFC.

  For text that is only ASCII the fold is ASCII lower-casing. Characters assigned
  after Unicode 15.0 may fold differently in implementations that carry newer
  tables; the conformance vectors do not use them.
- **search terms**: what stage 2 (§4.3) matches on. Take the search fold of the
  text. A **run** is a maximal sequence of characters that starts at a letter (L)
  or a number (N) and continues with letters, numbers and combining marks (M);
  marks that follow no letter or number belong to no run. A run that contains no
  character of the **unspaced scripts** is one term. The unspaced scripts are those
  written without spaces between words: U+0E00 to U+0EFF (Thai, Lao), U+1000 to
  U+109F (Myanmar), U+1780 to U+17FF (Khmer), U+2E80 to U+2FDF, U+3000 to U+312F,
  U+31A0 to U+31BF, U+31F0 to U+31FF, U+3400 to U+4DBF, U+4E00 to U+9FFF (Han, kana,
  Bopomofo), U+AC00 to U+D7AF (Hangul syllables), U+F900 to U+FAFF and U+20000 to
  U+3134F. In a run that has some, each maximal stretch of such characters is cut
  into **units** (a character plus the combining marks after it; a mark takes the
  class of the character before it) and gives, for unit *i* of *n*, the unit and,
  if *i* + 1 < *n*, the two units *i* and *i* + 1 joined; the stretches around it
  are terms of their own, in order. `拉巴斯` gives `拉`, `拉巴`, `巴`, `巴斯`, `斯`,
  so a query `巴斯` and a query `拉` find it and `巴拉` does not; `iPhone手机` gives
  `iphone`, `手`, `手机`, `机`. Dictionary segmentation is out of scope: pairs are
  the portable choice. The terms are the result without the stopwords, which are
  folded the same way (so `cual` is a stopword because `cuál` is). Symbols,
  punctuation, emoji and other characters that are neither letters, numbers nor
  marks separate runs and are not terms. Stage 1 does **not** use the search fold:
  its content tokens and token set key are exactly as above, because a wrong compiled
  answer cannot be detected afterwards (§4.2).

Only the matching is folded. A record's `title` and `body` are stored as they
were written; writers SHOULD still store them in NFC, so that what a person
reads and what a program compares are the same bytes. The search terms are
derived from the records when a database is opened and are never written to the
file, so changing this rule changes no `.fds` file and needs no reindexing.

`conformance/normalization.json` lists inputs and the results to reproduce,
including text in NFD and in scripts that depend on marks.

### 4.2 Stage 1 — compiled records (facts and procedures)

Try, in order, and stop at the first match:

1. **exact** — canonical query equals a record's canonical `question`
2. **normalized** — the content-token *sets* are equal (word order and
   stopwords ignored)
3. **semantic** — only if the database has embeddings: cosine similarity
   ≥ τ (default 0.60) **and** every content token of the record's question
   appears in the query **and** they share at least 2 content tokens

The coverage condition in step 3 is REQUIRED. Without it, embeddings treat
"pin 2" and "pin 4", or "minimum" and "maximum", as the same question and
return a wrong compiled answer. A missed match only falls through to stage 2;
a false match returns a wrong answer that nothing downstream can detect, so
implementations MUST favour precision here.

On a match, the result is that single record with confidence `exact`, and
stage 2 is skipped.

This rule is about *which words count*, and it does not understand meaning.
Content tokens still ignore question words and auxiliaries such as `who`,
`what`, `how`, `can` and `should`, so two questions that differ only by one of
them ("Can users delete records?" and "Should users delete records?") have the
same token set key and the same stage-1 answer. A deployment that needs to tell
them apart must record them as separate questions in separate sectors, or
review what stage 1 answers.

### 4.3 Stage 2 — retrieval over all records

Stage 2 ranks **every** record, not only knowledge. Facts and procedures that
did not match in stage 1 compete as *candidates*, indexed on their question,
title and body. Without this, a paraphrased intent ("sign up a client") could
never reach its procedure ("register a new customer"), which has no knowledge
fragment to fall back on. A compiled record returned from stage 2 is never
`exact`; its confidence follows §5 like any other record.

1. Filter by `collection` if given.
2. Rank with BM25 (k1 = 1.5, b = 0.75) over **search terms** (§4.1), so case and
   accents do not matter; keep the top 5. A record is indexed on the search terms of
   its body, of its title and, for facts and procedures, its question, and of its
   `source`. The title and the question describe what the record is about, so their
   terms count more than a term of the body (the reference tool counts them three
   times); a section is found by its heading even when the body never repeats it,
   and a file name or path finds the sections of that file. The length
   normalisation never treats a record as shorter than half the average one (the
   reference tool's choice), so a one-line section that only points at a table does
   not outrank the table.
3. **The keyword gate.** If BM25 matched nothing, stop: return `none` with no
   records. This step is REQUIRED. Vector search always returns the nearest
   record for any input whatsoever, so a pipeline that lets vectors decide
   *whether there is an answer* can never report `none` — measured on 10
   out-of-domain queries, every one of which came back with records, all of
   them vector-only matches. BM25 finding nothing means the query shares no
   content word with any record, which is the honest signal for "not in this
   database". Vectors still rank, and still surface records BM25 ranked
   poorly; they no longer decide whether an answer exists.
4. If embeddings exist, rank by cosine similarity; keep the top 5.
5. Fuse with reciprocal rank fusion, k = 60.
6. Pack in fused order: add a record if it fits the remaining budget; the
   first record is always added even if it alone exceeds the budget.

Records of `type: context` take part in neither stage: a context is
instructions, not an answer (§4.5).

The gate trades recall for the ability to refuse. On the reference corpus it
rejected 10/10 out-of-domain queries, at the cost of refusing 7 of 135
in-domain ones (5.2%) whose phrasing shared no content word with the stored
question. Among the queries it does answer, recall@5 is unchanged. The trade
is deliberate: a false `none` escalates to whatever the caller does next,
while a false `strong` hands back the wrong procedure.

---

### 4.4 Links

A record links to another by naming it, as in Obsidian: `[[target]]` anywhere
in its `body`, or `[[target|shown text]]` where only the target counts, or an
entry in its `links` field. Links inside fenced or inline code are text, not
links.

A target resolves by canonical name (§4.1): first against the questions and
intents of facts and procedures, then against every record's title. A target
that names nothing is **kept, not rejected** — it resolves once a record with
that name exists, so links can be written before what they point to.

Links resolve when the database is read; nothing about them is stored except
the text. Implementations expose, for any record, its resolved links and the
records that link to it (its backlinks), and the whole set of links as a graph.

A query MAY ask for its results' links (`links: true`, §8). The linked records
are appended once each, one hop out, with `match: link`, within what is left of
the budget. Linked records are context, not evidence: **confidence is not
changed** by them.

### 4.5 Contexts

A context is a record of `type: context`: a name (its `title`), instructions
(its `body`), and links (§4.4) to the records that go with them. It exists so
that an agent loads everything a task needs in one call, instead of rediscovering
it piece by piece — and so that it does not read it again when nothing changed.

Loading a context returns its instructions and every record its links reach,
depth-first in link order. A linked record that is itself a context contributes
its own instructions and records; a cycle is followed once. Only the context's
own links are followed, not the links of the records it bundles: what a context
names is exactly what it loads, so its size stays predictable. Targets that name
nothing yet are listed as `missing`.

Every loaded context carries a **hash** of its content — the ids, titles,
bodies, types and attributes of everything in it, and its missing targets — and
**not** of dates, so re-verifying a record without changing it does not make
every agent read the context again. A caller that holds a context sends its
hash; if nothing changed, the answer is a few bytes instead of the bundle.

A context's id comes from its name (§3.1): saving a context under the same name
replaces it, and renaming one is creating another.

## 5. Confidence

Every result carries exactly one confidence value:

| Value | Meaning |
|---|---|
| `exact` | A fact or procedure matched (§4.2) |
| `strong` | The first record contains every search term of the query (§4.1), in its body, title, question or source |
| `weak` | Records were returned but the first does not meet the `strong` condition |
| `none` | The keyword gate refused the query (§4.3 step 3); no records |

Confidence is data. What to do with it is the caller's decision.

`strong` once also meant "some returned record was found by both
retrievers". Across four domains that rule was wrong in 16–28% of its answers,
and it called out-of-domain questions `strong`, because with vectors on some
record nearly always has both signals. Restricted to full coverage of the
first record, `strong` was right in 97–100% and no out-of-domain question
reached it (`research/confidence_rules.py`). Callers that execute what they
receive should still prefer `exact`; `weak` is for callers that reason, ask,
or escalate.

Implementations MUST NOT collapse confidence into the fused score. RRF ranks
by position and discards magnitude: on the reference corpus the fused scores
of right and wrong top-1 results overlap completely (right: 0.0323–0.0328,
wrong: 0.0315–0.0328), so no threshold on that score separates them. This is
why §8 exports the raw retriever signals instead.

---

## 6. File format (`.fds`, format version 4)

A database is one file. This section describes it byte by byte, so that it can
be implemented without reading the reference code: `conformance/fds.py` is a
second implementation written from this text alone, and both must read the
same files the same way (§13).

Two numbers are easy to confuse. The **format version** (`major.minor`, now
4.0) is written in every file and says how to read it. The **version of this
document** says which draft of the specification you hold. They move
independently.

A record is a CBOR map (RFC 8949), so any language reads it with a library it
already has. The file around the records is a fixed 64-byte header and an
index. All integers in the header and the index are little-endian; integers
inside CBOR are big-endian, as the RFC says.

### 6.1 Writes

The file is an immutable artifact. Every write MUST produce a complete new file
at a temporary path and then atomically replace the old one. A reader holding
the old file sees a consistent snapshot until it reopens.

The temporary path is unique to each write (`<file>.<pid>.<time>.<n>.tmp`), so
two writes can never truncate each other's work. On Windows, a reader that has
the file open or mapped makes the replace fail for a moment; the writer retries
for up to three seconds.

### 6.2 Versions and compatibility

- **C1.** A reader MUST refuse a file whose `format_major` differs from the one
  it implements, with the category `unsupported_major` (§6.6).
- **C2.** A reader MUST read every minor of its major. It MUST ignore the
  reserved header bytes, and any key it does not know in the metadata or in a
  record, whatever the type of its value.
- **C3.** A writer MUST NOT rewrite a file whose version is newer than its own
  (a larger major, or the same major and a larger minor). Rewriting would drop
  what the writer cannot see. It refuses with an error that names the version.
  Reading such a file is allowed: a newer tool's database can be queried by an
  older tool, and not changed.
- **C4.** A change that an older reader can ignore (a new optional record key,
  a meaning given to the reserved bytes) raises the **minor**. A change it
  cannot ignore (the header, the meaning of an existing key, the encoding)
  raises the **major**.
- **C5.** The set of `kind` values is closed within a major (§2). An unknown
  `kind` is not an unknown key: the record is malformed.
- **C6.** A writer that rewrites a file SHOULD keep `application_id` and
  `user_version`. The reference tool writes zero in both and does not yet keep
  them.

Earlier generations are described in §6.5. A reader SHOULD read them; a writer
MUST write the current one, so a database migrates the first time it is saved
and not before: reading one changes nothing on disk.

### 6.3 Layout

```
offset  size  field            value
     0     8  magic            89 46 44 53 0D 0A 1A 0A      ("\x89FDS\r\n\x1a\n")
     8     2  format_major     u16   4
    10     2  format_minor     u16   0
    12     4  application_id   u32   0 unless an application claims the file
    16     4  user_version     u32   the application's own number; 0 otherwise
    20     4  record_count     u32
    24     8  meta_offset      u64   64
    32     4  meta_length      u32
    36     8  index_offset     u64
    44     4  meta_crc32       u32   CRC-32 of the metadata bytes
    48     4  index_crc32      u32   CRC-32 of the index bytes
    52     8  reserved                zero; readers ignore them (C2)
    60     4  header_crc32     u32   CRC-32 of bytes 0..60
    64        metadata         one CBOR map                  (meta_length bytes)
              records          record_count CBOR maps, back to back
              index            record_count x 16 bytes
```

Each index entry is `offset u64`, `length u32`, `crc32 u32`: where one record
starts, how long it is, and the CRC-32 of its bytes. Entry *i* describes the
*i*-th record, so any record is readable without reading another.

The magic follows PNG's reasoning: a first byte outside ASCII shows that a
transfer stripped the high bit, and `\r\n`, `\x1a` and `\n` show that something
translated line endings. Either breaks the magic, and the file is refused
instead of misread.

**CRC-32** is the one of zlib and IEEE 802.3: reflected polynomial `0xEDB88320`,
initial value and final XOR `0xFFFFFFFF`. Its check value is `0xCBF43926` for
the nine bytes `123456789`.

A writer MUST lay the file out exactly as drawn: the metadata right after the
header (`meta_offset` = 64), the records in index order with no gap, the index
last. A reader MUST NOT depend on that: it finds each region by the header and
the index, and takes the records region to be everything between the end of the
metadata and `index_offset`.

### 6.4 Metadata and records

The metadata is a CBOR map with text keys: `name`, `created`, `updated`,
`embedding_model` (empty if none) and `embedding_dims`, an unsigned integer. A
writer MUST write all five. A reader takes an absent one as empty, or 0.

A record is a CBOR map with text keys:

| Key | CBOR type | Written | Absent means |
|---|---|---|---|
| `id` | text | always | (error) |
| `kind` | text: `fact`, `procedure` or `knowledge` | always | (error) |
| `collection` | text | always | (error) |
| `question` | text (a reader also accepts null) | when the record has one, even if it is `""` | no question |
| `body` | text | always | (error) |
| `title` | text | always | (error) |
| `source` | text | always | (error) |
| `verified_by` | text | always | (error) |
| `last_verified` | text, `YYYY-MM-DD` | always | (error) |
| `tokens` | unsigned integer, at most 2³²−1 | always | (error) |
| `embedding` | byte string: the vector's `f32` values, little-endian, back to back; a multiple of 4 bytes | when the record has one, even if empty | no embedding |
| `type` | text | when not empty | `""` |
| `attrs` | map from text to text | when not empty | no attributes |
| `links` | array of text | when not empty | no links |

**Writers MUST** encode so that equal records are equal bytes in any language
(RFC 8949 §4.2.1, deterministic encoding):

- **W1.** Definite lengths only, and the shortest head for every length and
  integer (23 is one byte, 24 is two).
- **W2.** Map keys sorted by their encoded bytes, ascending: a shorter key
  before a longer one, then bytewise. This is the order of `attrs` too.
- **W3.** Optional keys written exactly as the table says, so that "absent",
  "empty text" and "empty vector" stay three different things.
- **W4.** Text is UTF-8. Writers SHOULD store it in Unicode normalisation form
  NFC (§4.1).

Values this version never writes itself, such as floats or booleans, can exist
only inside keys it does not know, and are skipped.

**Readers MUST**:

- **R1.** Accept the keys in any order, and any integer width.
- **R2.** Refuse as malformed a missing required key, a key twice (also inside
  `attrs`), a value of the wrong type, text that is not valid UTF-8, an
  embedding whose length is not a multiple of 4, an integer that does not fit
  where the table says, and bytes after the end of the map.
- **R3.** Refuse indefinite-length items, anywhere, including inside the values
  of keys they skip. Skipping tolerates tags and every other type.
- **R4.** Refuse nesting deeper than 32 levels. A file must not be able to
  exhaust a reader's stack.
- **R5.** Never trust a length before checking it against the bytes that are
  left: an array that claims 2⁶⁴ elements is an error, not an allocation.

### 6.5 Earlier generations

**Version 3** (`FDS3`) and **version 2** (`FDS2`) were written before this
specification was a document, and encoded each record with `bincode`, which is
positional and tied to Rust's types: a new field changed the bytes of every
record. That is why version 4 replaced it. They are frozen. The reference tool
reads them (version 2 gives each record an empty `type`, no `attrs` and no
`links`), and no other implementation needs to. Version 1 (`FDS1`) is not read.

### 6.6 Reading: what a damaged file is

Damage is an error and never a wrong answer. A reader MUST report one of five
categories, and MUST check in this order, so that two implementations name the
same one:

| Category | Meaning |
|---|---|
| `not_fds` | not a file of this format |
| `truncated` | the file ends before something it says is there |
| `unsupported_major` | a major version this reader does not implement |
| `checksum` | a CRC-32 does not match: the bytes changed |
| `malformed` | the bytes are not what this section says |

On opening, in order:

1. Let *n* be the smaller of the file's length and 8. If the first *n* bytes are
   not the first *n* bytes of the magic: `not_fds`. (So an empty file, or a copy
   that stopped in the middle of the magic, falls to the next check.)
2. The file is shorter than 10 bytes: `truncated`.
3. `format_major` is not the reader's: `unsupported_major`.
4. The file is shorter than 64 bytes: `truncated`.
5. `header_crc32` does not match bytes 0..60: `checksum`.
6. The metadata: `meta_offset + meta_length` is past the end of the file:
   `truncated`; `meta_offset` is below 64: `malformed`; its CRC does not match:
   `checksum`.
7. The index: `index_offset + 16 × record_count` is past the end of the file:
   `truncated`; `index_offset` is below the end of the metadata: `malformed`;
   its CRC does not match: `checksum`.
8. The metadata does not decode: `malformed`.

Offsets are 64-bit and come from the file: sums MUST be checked so that they
cannot overflow.

Reading record *i*, in order: its entry points outside the records region
(before the end of the metadata, or past `index_offset`): `malformed`; the CRC
of its bytes does not match the entry: `checksum`; it does not decode:
`malformed`. A damaged record does not stop the others from being read.

A file cut short at any length is `truncated`.

### 6.7 Concurrent writers

A write reads the file, changes the records and saves a new file. Two writers
doing that at once would both start from the same copy, and the second would
erase the first. Every writer -- the CLI, the server, an agent through MCP --
therefore holds an exclusive OS lock on `<file>.lock` from before it reads
until after it replaces the file. A writer waits up to `FDS_LOCK_TIMEOUT_SECS`
(default 60) and then gives up with an error; it never writes without the lock.

Readers take no lock. A long-lived reader (the HTTP server, an MCP session)
keeps the file's size and modification time and re-reads the file before a
request when either changed, so it never answers from a copy older than the
file.

---

## 7. Command-line interface

The reference tool is `fds`. Commands operate on the database named with
`--db <file>`, or else on the single `*.fds` file in the current directory.
Data goes to stdout; messages go to stderr; every command accepts `--json`
where it prints data. Exit status is 0 on success, 1 on error, 2 on a query
that returned nothing.

```
fds init [name]                    create an empty database
fds status                         counts by kind and collection, embeddings, size

fds add <path>...                  ingest files or folders as knowledge;
                                   images are described (§3.4):
                                   [--vision-model M] [--lang CODE]
fds fact add <question> <answer>   add or replace a verified fact
fds proc add <file.json>           add or replace a procedure
fds context ls                     list contexts with size and hash
fds context show <name> [--known H]  load a context; one line if H is current
fds context set <name> [--instructions T | --file F] [--link TARGET]...
fds context import <file.md> [--name N]   load AGENTS.md or any Markdown as a context
fds context export <name>          print a context as Markdown (AGENTS.md)
fds ls [--kind K] [--type T] [--collection C]   list records
fds show <id>                      print one record with its provenance
fds rm <id> | --source <path>      remove records

fds embed [--model M]              compute and store embeddings (needs Ollama)
fds query <text> [--links] [--verified-by human,model:]   run a query (§4)
fds import <file> [--dry-run] [--skip-errors] [--kind K]
                                   import rows from CSV, TSV, JSON or JSON Lines
                                   (§8.3); exits 1 if errors blocked the import

fds serve [--port P] [--clearance a,b]
                                   HTTP API, manager and MCP (default port 7070)
fds serve --sectors <manifest>     every sector, one manager each, one /mcp (§10)
fds mcp [--write] [--as NAME] [--clearance a,b]
                                   MCP over stdio; --write adds the memory tools
fds mcp --sectors <manifest> --as <agent>   an agent's sectors, without a server
fds mcp --connect <url>            bridge stdio to a running `fds serve`;
                                   the agent key comes from FDS_KEY

fds log <summary> [--why T] [--file P]... [--branch B] [--commit C] [--by AGENT]
        [--role R] [--status S] [--to WHO]
                                   record what was done (§3.5); branch and commit
                                   default to the git checkout in this directory
fds journal show [--md] [--timeline] [--branch B] [--agent A] [--role R] [--since DATE]
                 [--limit N] [--sectors M] [--audit FILE]
                                   --timeline: the black box, numbered, oldest first (§11.4)
fds journal export [--out FILE]    the journal as a Markdown page (default JOURNAL.md)
fds journal publish [--repo DIR] [--to BRANCH]
                                   commit the page to a branch (default `fds`), §7.2

fds policy add <text> --subject S [--action A] --rule deny|range|one_of
        [--min N --max N] [--values a,b] [--unit U]
        [--tool T [--tool-subject S] [--tool-action A] [--tool-value V]]
fds policy ls                      the policies, and which ones do not count (§11.1)
fds check --subject S --action A [--value V] [--sectors M] [--default allow|deny]
                                   allow (exit 0) or deny (exit 3), with no model (§11.2)
fds guard --policies M [--agent A] [--audit F] -- <the real MCP server>
                                   check every tool call, record every decision (§11.3)
fds audit show|verify [--file F]   read the audit; check its chain (§11.4)

fds sector new <name> [--agent] [--reads A,B] [--file F] [--clearance a,b]
fds sector ls                      sectors, roles, reads, record counts
fds sector key <name>              new key for an agent; the old one stops working
fds sector promote <id> --from A --to B   move a record, now verified by a person
```

### 7.1 Ingestion

`fds add` walks folders, skipping hidden directories and `target`,
`node_modules`, `.venv`. Fragments are made per file type:

- Markdown: one fragment per heading section
- Plain text: paragraphs, merged up to about 200 words
- Source code: windows of 40 lines
- Images (JPG, PNG, WebP, GIF, BMP): one `type: image` record each (§3.4)
- Anything else, or binary: skipped

Re-adding a path replaces that path's previous fragments.

### 7.2 Publishing the journal

`fds journal publish` writes two files, `JOURNAL.md` and `journal.json`, as a
commit on one branch of a git repository (default branch `fds`, default
repository the current directory). It uses git's plumbing — it writes the objects
and moves the branch reference — so **nothing else changes**: not the working
tree, not the index, not the branch that is checked out, whatever is
uncommitted. It refuses when the target branch is the one checked out. Publishing
the same journal again commits nothing; a changed one adds a commit whose
parent is the previous one. It never pushes: sharing the branch is the owner's
decision (`git push origin fds`).

`fds journal publish` needs `git` on the PATH. It is the only part of the tool
that runs another program.

---

## 8. HTTP API

| Method | Path | Body | Returns |
|---|---|---|---|
| `GET` | `/status` | — | database summary |
| `POST` | `/query` | `{"q": "...", "collection": "...", "budget": 400, "links": false, "verified_by": "human"}` | query result |
| `POST` | `/check` | `{"subject": "...", "action": "...", "value": "-18"}` (`value` optional; a number is accepted) | the decision of §11.2; advisory |

A query result is:

```json
{
  "query": "what is the reference voltage of the lm317",
  "confidence": "exact",
  "records": [
    {
      "id": "a41f09c2e7b3d511", "kind": "fact", "collection": "electronics",
      "question": "what is the reference voltage of the lm317",
      "body": "1.25 V, maintained between the OUT and ADJ pins.",
      "source": "handbook/lm317.md", "verified_by": "human",
      "last_verified": "2026-09-17", "match": "exact",
      "score": 1.0, "bm25": 0.0, "cosine": 0.0
    }
  ],
  "used_tokens": 8, "budget": 400
}
```

`match` is one of `exact`, `normalized`, `semantic`, `keyword`, `vector`,
`keyword+vector`.

### 8.1 Exported features

Every record carries three numbers, and they are not interchangeable:

| Field | What it is |
|---|---|
| `score` | The fused rank score. Good for ordering, poor for thresholding (§5) |
| `bm25` | Raw BM25 score, or `0.0` if that retriever did not reach the record |
| `cosine` | Raw cosine similarity, or `0.0` if there was no query embedding |

`bm25` and `cosine` are REQUIRED. They exist so a caller can calibrate its own
threshold, and so a reranker can be trained on real traffic without FDS having
to guess what a caller considers confident. A query result plus the caller's
own judgement of which record was right is a complete training example: the
records FDS ranked above the right one are the hard negatives.

`0.0` means "this retriever did not reach this record", which is information —
it is not the same as a low score.

### 8.2 Management

An implementation MAY also expose the database for administration. If it
does, it uses these routes, so that one manager works against any
implementation:

| Method | Path | Body | Returns |
|---|---|---|---|
| `GET` | `/records?kind=&type=&collection=&search=&offset=&limit=` | — | `{total, offset, limit, rows}`; bodies truncated to a preview |
| `GET` | `/records/{id}` | — | one full record with resolved `links`, `links_explicit` and `backlinks`; `id` may be a prefix of 6+ characters |
| `PATCH` | `/records/{id}` | `{"body"?, "title"?, "type"?, "attrs"?, "links"?, "verify"?}` | `{"id"}` — see below |
| `POST` | `/facts` | `{"question", "answer", "collection"?, "source"?}` | `{"id"}` |
| `POST` | `/procedures?collection=` | a procedure object, or an array of them (§3.2) | `{"ids"}` |
| `DELETE` | `/records/{id}` | — | `{"removed"}` |
| `POST` | `/embed` | `{"all"?, "model"?}` | `{"embedded", "status"}` |
| `POST` | `/import` | rows and documents, §8.3 | the import plan; `422` if refused |
| `GET` | `/contexts` | — | every context: name, size in records and tokens, hash |
| `GET` | `/contexts/{name}?known=` | — | the context (§4.5) and an `ETag`; `{"changed": false}` if `known` is its hash, `304` on a matching `If-None-Match` |
| `POST` | `/contexts` | `{"name", "instructions"?, "links"?, "collection"?}` | `{"id"}`; replaces a context of the same name |
| `GET` | `/graph` | — | `{nodes, edges}`: every record, and every resolved link as a pair of node indexes |
| `GET` | `/export?format=&delimiter=&kind=&collection=` | — | a file in the shape `/import` reads |
| `POST` | `/reload` | — | database summary, after re-reading the file |

Errors are `{"error": "..."}` with a 4xx or 5xx status. A batch of
procedures with one invalid entry is rejected whole: nothing is written.

**An edit is a verification.** `PATCH` is how a person corrects or confirms a
record, so any accepted patch — even `{"verify": true}` alone — sets
`verified_by: human` and `last_verified` to today. If `body` changes, the
record's vector no longer describes it and is removed until embedded again. A
question or intent cannot be patched, since the id is made from it; neither can
a context's name, nor can a record become a context or stop being one. A
procedure's new body must still be valid (§3.2). An empty patch is refused.

**Every write reads the file first.** The read, the change and the save
happen under one lock, starting from what is on disk rather than from memory.
A record added from the command line while the manager is running is kept,
not overwritten by a stale copy. Two separate processes can still race in the
milliseconds between one's read and its save; v1 does not lock the file.

**Writes are authenticated, because a loopback server is reachable from any
web page.** A page on any site can send requests to `127.0.0.1`, and a
manager that writes on request would let it edit the database. So:

1. Listen on loopback only.
2. Reject any request whose `Host` is not the loopback address and port the
   server is bound to. This defeats DNS rebinding, in which an attacker's
   domain resolves to `127.0.0.1` and so becomes same-origin in the browser.
3. Require `x-fds-token` on every write except `/query`, `/check` and `/reload`, which
   only read. The token is minted at startup, printed on the terminal, and
   placed only in the page the server itself serves. A foreign page cannot
   read that page, because the server sends no CORS headers.
4. Serve the manager page with `Cache-Control: no-store` and
   `Content-Security-Policy: frame-ancestors 'none'`, since it carries the
   token and holds a delete button.

A manager MUST insert record content as text, never as markup. Records are
written by whoever has write access and read by everyone who opens the
manager; a fact whose answer is HTML must display as that HTML, not run it.
The reference manager was checked by hand against exactly that — a fact whose
answer is an `<img onerror=…>` payload renders as text and creates no element —
but that check is not yet an automated test.

### 8.3 Import and export

`POST /import` loads many records at once. It is the same operation whether
it previews or writes: `dry_run: true` returns the plan and stops; `false`
returns the same plan and applies it. A client MUST NOT compute its own
preview — the plan the server returns is the one it then carries out.

```json
{
  "dry_run": true,
  "on_error": "abort",
  "collection": "electronics",
  "source": "preguntas.csv",
  "rows": [
    {"kind": "fact", "question": "What is pin 4 of the NE555?", "answer": "RESET"},
    {"kind": "procedure", "intent": "flush the dns cache", "command": "ipconfig /flushdns", "shell": "cmd"}
  ],
  "documents": [{"name": "handbook/lm317.md", "text": "# LM317\n..."}]
}
```

- **Rows** are facts or procedures. `kind` is `fact` or `procedure` (`hecho`
  and `procedimiento` are accepted). Facts need `question` and `answer`.
  Procedures need `intent` and either `steps` — an array, or JSON text of one,
  as a spreadsheet cell holds it — or a `command` (with optional `shell`),
  which becomes a single `run` step. `collection`, `source`, `verified_by` and
  `last_verified` are optional per row; the request's `collection` and
  `source` fill in what a row leaves out.
- **Documents** are split into knowledge exactly as §7.1 splits files, so
  importing a file and running `fds add` on it give the same records.
- **Contexts** can be rows too: `kind: context` with a `name` (or `title`),
  `instructions` (or `answer`) and `links`.
- Any fact or procedure row may carry `type`, `attrs` (an object, or its JSON
  text) and `links` (an array, its JSON text, or a list separated by `;` or new
  lines). They are never stored inside a procedure's body.
- **Images** are documents with `data` (base64) and optionally `media_type`
  instead of `text`. The request may name a `vision_model` and a `lang` to
  describe them in (§3.4). A preview (`dry_run`) never calls the model; a real
  run describes every new or changed image **before** writing anything, so under
  `abort` one failed description writes nothing at all.

Every row and document gets one status:

| Status | Meaning |
|---|---|
| `new` | no record has this id |
| `replace` | a record has this id and different content; `arg` says which part differs |
| `unchanged` | a record has this id and identical content; **it is not rewritten**, so it keeps its `last_verified` |
| `duplicate` | a later row in the same import has the same id; the later row wins, as a spreadsheet read top to bottom |
| `error` | the row cannot be imported; nothing about it is written |

Each non-trivial status carries a stable `code`, an optional `arg`, and a
`message` in English. Clients show the code in their own language:
`duplicate` (arg: the winning row), `replace` (arg: `content`, `collection`,
`title` or `source`), `missing_kind`, `unknown_kind`, `knowledge_row`,
`missing_question`, `missing_answer`, `missing_intent`, `missing_steps`,
`bad_steps`, `missing_name` (a context row), `invalid` (arg: the underlying
reason), and for documents `too_large`, `binary`, `unsupported` (arg: the
extension), `empty`. Images add `describe` (arg: the model that will describe
it, in a preview), `described` (arg: the model that did), `vision_failed` (arg:
the model's error), `vision_unavailable` and `bad_image_data`. `replace` can
also name `links` or `attributes` as the part that differs.

`on_error` decides what errors do. `abort`, the default, writes nothing if
any row fails and answers `422` with the plan; a batch lands whole or not at
all. `skip` writes the valid rows and reports the rest.

`GET /export?format=json|csv&delimiter=comma|semicolon&kind=&collection=`
writes matching records **in the shape `/import` reads**. The guarantee:
exporting and importing the result reports every record `unchanged`.

- **JSON** carries every kind; knowledge is included for reading and backup,
  and is rejected as a row on import (`knowledge_row`) — re-import the
  source documents instead.
- **CSV** carries facts, procedures and contexts, one per line, with columns
  `kind, question, answer, intent, title, steps, type, attrs, links,
  collection, source, verified_by, last_verified` (`steps`, `attrs` and
  `links` as JSON; a context's name in `title`, its instructions in `answer`),
  RFC 4180 quoting, CRLF,
  and a UTF-8 byte-order mark so spreadsheets read accents correctly.
  `delimiter` exists because spreadsheet programs split CSV on the system's
  list separator: `;` across continental Europe and Latin America, `,`
  elsewhere.
- **Formula guard.** A CSV cell beginning with `=`, `+`, `-`, `@`, tab or
  carriage return is written with a leading `'`, so a record can never plant a
  formula in someone's spreadsheet. A CSV reader importing into FDS MUST strip
  exactly that apostrophe — one `'` followed by one of those characters — so
  an answer like `-5 V` survives the round trip.

## 9. MCP

FDS's tools, over JSON-RPC 2.0:

- `fds_query` — the §4 query; `with_links: true` adds linked records (§4.4),
  `verified_by` restricts them by provenance
- `fds_contexts` — every context, with its size and hash
- `fds_context` — load a context (§4.5); given `known_hash` equal to the current
  hash, the answer is one line saying so, not the bundle
- `fds_check` — ask whether an action is allowed before doing it (§11.2):
  `subject`, `action`, optional `value`. Advisory; the guard enforces.
- `fds_status` — the database summary
- `fds_journal` — read the change journal (§3.5), read-only and always available:
  optional `limit` (default 50, the newest), `agent`, `role`, `branch`, `since`
  (`YYYY-MM-DD`) and `timeline` (default true). With `timeline` the numbered lines of
  `fds journal show --timeline`, oldest first, then each entry's detail (why, files,
  branch, commit, who wrote it, whether a person reviewed it); without it, the format
  of `fds journal show`, newest first. It reads every sector the caller reads, naming
  the sector, and a record the caller is not cleared for (§11.4) appears as its
  stand-in. No entries: "el registro está vacío", not an error.

When the connection has a memory sector (`fds mcp --write`, a bearer token on
one database, or an agent key with sectors), three more:

- `fds_remember` — save a fact (`question`, `answer`) in the agent's own
  sector, with `verified_by: model:<agent>` and `source: agent:<sector>`.
  The same question again replaces the earlier answer.
- `fds_log` — write a journal entry (§3.5): `summary` (required), `why`, `files`,
  `branch`, `commit`, `role`, `status`, `to`. It is stored as `model:<agent>` and waits for a person.
  The tool's description tells the agent to write for someone who never saw its
  session, and not to paste code or logs.
- `fds_forget` — remove, by id, a record an agent wrote. A record whose
  `verified_by` is not `model:…` is refused: what a person verified stays.

Results are rendered as text and — per §1 — contain no instructions to the
model. A context's instructions are the owner's words, returned as data like
any other body.

### 9.1 Protocol versions

The server answers both eras of MCP. A request whose `_meta` carries
`io.modelcontextprotocol/protocolVersion` (revision 2026-07-28) is served
statelessly, with `server/discover` available. An `initialize` request
(revisions 2024-11-05 to 2025-11-25) is answered with the client's version
when supported, else 2025-06-18. Supported: 2026-07-28, 2025-11-25,
2025-06-18, 2025-03-26, 2024-11-05.

### 9.2 Transports

`fds mcp` speaks stdio: one JSON-RPC message per line. `fds serve` answers
`POST /mcp` (Streamable HTTP): one message per request, one JSON object per
answer, `202` for a notification, `405` for GET and DELETE, no session.

- A request with an `Origin` other than the server's own loopback origin is
  refused with 403, so a web page cannot drive a local agent's memory.
- A modern request must carry `MCP-Protocol-Version`, `Mcp-Method` and, for
  `tools/call`, `Mcp-Name`, all matching the body (`=?base64?…?=` decoded);
  otherwise 400 with error -32020.
- On one database, reads need nothing; the memory tools need
  `Authorization: Bearer <token>`, the token `fds serve` prints.
- With sectors (§10), every request needs `Authorization: Bearer <agent key>`;
  without a valid key the answer is 401.

`fds mcp --connect <url>` is a bridge for clients that only launch stdio
servers: it loads no data and forwards each line to the server, with the key
from `FDS_KEY` (never from the command line, where other processes can read
it).

---

## 10. Sectors

A sector is one database file with a name. Agents get sectors of their own:
each keeps its memory in its own file, reads the shared sectors it is given,
and cannot write anywhere else. The layout is a manifest, `sectors.json`, in
the folder of the files:

```json
{ "sectors": [
    { "name": "shared", "file": "shared.fds" },
    { "name": "sales",  "file": "sales.fds", "reads": ["shared"], "key_sha256": "…" } ] }
```

- Names are lowercase letters, digits, `-` and `_`, up to 40, unique. Every
  `reads` entry must exist and must not be the sector itself. A manifest that
  breaks a rule is refused whole.
- Two optional keys carry the guard's and the sensitivity rules (§11): a
  manifest-level `"default": "allow" | "deny"`, and a per-sector `"clearance":
  ["personal", …]` listing the sensitivity levels that sector's agent may read in
  full.
- A sector with `key_sha256` is an agent's home. `fds sector new <name>
  --agent` mints the key (`fdsk_` and 32 hex digits), prints it once, and
  stores only its SHA-256. `fds sector key` replaces it.
- An agent's view is its own sector, then its `reads`, in order. A query runs
  in each and the answers merge: the overall confidence is the best any sector
  reached; records are taken by turns, best sector first and the agent's own on
  a tie, within one token budget, each labelled with its sector. Scores are not
  compared across sectors: each file has its own word statistics. Links and
  contexts resolve within their own sector.
- `fds_remember` writes only to the agent's own sector, as `model:<agent>`.
- `fds sector promote <id> --from A --to B` moves a record into B with
  `verified_by: human`. It holds both locks (§6.7), taken in a fixed order, and
  saves B before removing from A: a crash in between leaves the record twice,
  never nowhere.
- `fds serve --sectors` loads each file once and serves each sector's manager
  and API at `/s/<name>/`, an index at `/`, and one `/mcp` for every agent. It
  re-reads the manifest when it changes, so a rotated key is refused on the
  next request. A sector added while it runs needs a restart.

Measured with five agents over a shared base of 10,000 records: five
`fds mcp` processes held 300 MB; one `fds serve --sectors` and five bridges
held 110 MB, at 6 ms per query (`research/sectors_ram.py`).

---

## 11. Policies, the guard, the audit and sensitive data

An agent can be persuaded; a comparison of numbers cannot. FDS keeps the rules
a system must never break as records a person verified, and answers whether an
action breaks one without any model. A separate guard stands between an agent
and the real tool, asks before every action, and writes down what it decided. A
model may explain a refusal; it never holds the key.

What this section does and does not give you is in §11.6. In short: it decides
and records the calls that pass through the guard, and it hides what an agent is
not cleared for. It is not a sandbox: an agent that can reach the tool another
way is not guarded.

### 11.1 Policy records

A policy is a fact with `type: "policy"`. Its body is the rule in the owner's
words; its attributes make it checkable:

| attr | meaning | example |
|---|---|---|
| `subject` | what it protects; a trailing `*` matches a prefix | `fridge.chamber3.setpoint`, `db.prod.*` |
| `action` | the operation, or `*` (the default) | `set`, `delete`, `drop`, `read_raw` |
| `rule` | `deny`, `range` or `one_of` | `range` |
| `min`, `max` | bounds for `range`, inclusive | `-20`, `-16` |
| `values` | comma-separated, trimmed, for `one_of` | `copy,staging` |
| `unit` | shown in answers | `°C` |
| `guard` | how a tool call becomes a check (§11.3), as JSON | see below |

**Only policies verified by a person count** (`verified_by: human`). One a model
wrote is ignored by every check, wherever it is stored: a compromised agent
cannot loosen a rule by writing one. Policies normally live in a sector every
agent reads and none can write (§10).

**A number** is a finite decimal in ASCII: an optional sign, digits with an
optional fraction, an optional exponent (`-18`, `+.5`, `1e3`). `inf`, `NaN`,
underscores, and digits of other alphabets are not numbers. White space around a
value is ignored.

`fds policy add <text> --subject S --rule R …` writes a policy and refuses what
could never work (a range whose `min` is above its `max`, a `one_of` with no
values). `fds policy ls` lists them and marks any that do not count.

### 11.2 The check

`fds check --subject S --action A [--value V]`, `POST /check` and the MCP tool
`fds_check` take the same three fields and answer:

```json
{ "decision": "deny", "subject": "fridge.chamber3.setpoint", "action": "set", "value": "-40",
  "because": [{ "id": "…", "rule": "range", "min": "-20", "max": "-16", "unit": "°C",
                "text": "Below -20 °C the stock is damaged.", "verified_by": "human",
                "last_verified": "2026-10-23", "reason": "-40 is outside -20..-16" }],
  "default_applied": false }
```

- Every counted policy whose `subject` and `action` match is evaluated.
  `deny` always denies; `range` denies a value outside `[min, max]`, a value
  that is not a number, and a missing value; `one_of` denies a value not in
  `values` and a missing value.
- **What cannot be evaluated is denied**: a `range` without usable bounds
  (bounds must be finite numbers too), a `one_of` without values, an unknown rule.
- One violated policy is enough: deny overrides allow. `because` lists the
  policies that denied; for an allow, the ones that matched and passed.
- No matching policy: the decision is the **default**, `allow` unless set, and
  `default_applied` says so. The default is the `default` of the manifest
  (`"deny"` for subjects that must never fail open) or `--default` on the CLI.
- The check is a pure function: the same policies and the same question always
  give the same answer. It reads files the way a query does (§6.7), so a policy a
  person just changed applies to the next check.
- `fds check` exits 0 on allow and 3 on deny.
- `POST /check` and `fds_check` are **advisory**: they judge by the policies the
  caller can read and apply the default `allow`. The guard enforces; where the
  answer says `default_applied`, it applies its own default.

### 11.3 The guard

`fds guard --policies <manifest> [--agent NAME] [--audit FILE] -- <command of the
real MCP server>` starts the real server as a child process and presents the
same tools to the agent over stdio. The agent's host starts `fds guard` in place
of the server, and the real server's credentials go in the guard's environment,
where the agent cannot read them.

Everything the agent sends goes through unchanged, except `tools/call`. For
that, the guard looks at the policies' `guard` attribute, a JSON object that
maps a call to a check:

```json
{ "tool": "set_temperature", "subject": "fridge.{chamber}.setpoint", "action": "set", "value": "{celsius}" }
```

`{name}` takes the call's argument of that name (a string, number or boolean; the
number as JSON writes it). Then:

- Every distinct check the mapped policies produce is run against **all** the
  counted policies. One denial denies the call.
- A call that lacks an argument a mapping needs is denied: it cannot be checked.
- A call to a tool no policy maps is forwarded under the default `allow` and
  refused under `deny` (`"default": "deny"` in the manifest).
- Allowed calls are forwarded; denied ones never reach the tool and come back as
  a tool error with the rule, its text, and a reminder that a rule decided it, not
  a model.
- Rules are re-read when a file changes: a policy a person adds applies to the
  agent's very next call. A file that cannot be read keeps the last good rules.

The guard forwards the message it parsed, written out again, never the line it
read. Two JSON parsers can disagree about a line (a key given twice, say), and the
tool must act on exactly what was checked. A line that is not JSON, or a batch
(an array), is refused and never forwarded, so a call cannot hide inside one.

A decision that cannot be recorded is not taken: if the audit cannot be written,
the call is refused.

### 11.4 The audit

Every `tools/call` the guard decides is appended to `audit.jsonl` (next to the
manifest unless `--audit`), one JSON object per line: `n` (1, 2, 3…), `prev`, `at`
(UTC), `agent`, `tool`, `args` (long strings cut at 400 characters), `decision`,
`reason`, and the `checks` made, each with its subject, action, value, decision
and the ids of the rules that decided. The file is only appended to, under a lock.

`prev` is the SHA-256, in lower-case hex, of the previous line exactly as
written (64 zeros for the first). A line that is changed, removed, inserted or
moved breaks the chain, and `fds audit verify` reports the first line where it
fails. `fds audit show` lists the decisions.

A chain proves that nothing in the middle was altered; it cannot show that the
last lines were cut off. `verify` prints the hash of the last line: store it where
the agent cannot write, and compare later. **The audit is tamper-evident, not
tamper-proof.**

`fds journal show --timeline` merges the guard's decisions with the journal's
entries (§3.5) in the order they happened, with the audit next to the manifest or
`--audit FILE`: the black box, recorded by the guard for what passes through it
and written by the agents for the rest.

### 11.5 Sensitive records

A record with `attrs.sensitivity` (`personal`, `secret`, any word the owner
uses) is returned in full only to a caller whose sector lists that level in its
`clearance` in the manifest (compared without regard to case). Any other caller
gets a stand-in: the record's `attrs.derived` text, written by a person (for
example "the customer lives in Lima"), or, without one, `[protected: personal]`.
The stand-in keeps the record's id and title and its `sensitivity` and
`protected` attributes; it loses its source, other attributes, links and vector.

The stand-in replaces the record **before anything is indexed** for that caller,
not after the answer is built. Searching for a word that only the hidden body
contains finds nothing, so nobody can probe what a record says by asking.

Where it applies: every door that knows who the caller is — `fds_query`,
contexts and every other MCP tool, over stdio (`fds mcp --sectors --as`) and over
HTTP (`/mcp` with an agent's key). For one database, `--clearance a,b` on
`fds mcp` and `fds serve` grants levels to every agent; without it, nothing
sensitive is shown.

Where it does not apply: the owner's doors. The command line, the manager, the
export and the HTTP API at `/query` and `/s/<sector>/` serve the whole file,
because whoever holds the file holds everything in it. They are not protected
against an agent that can reach them: keep the server on loopback and the agent
where it cannot connect to it. Ids and titles are not hidden, so keep
sensitive data out of them.

### 11.6 What this is, and is not

- **It guards what passes through the guard.** An agent with another path to the
  tool (its own credentials, a shell) is not guarded. The guard holds the
  credentials; that is what makes it the only path.
- **It is not a sandbox, and not a firewall.** It compares what it was told to
  compare. A tool whose arguments do not carry the thing a rule is about cannot
  be checked.
- **The audit shows tampering; it does not prevent it.** §11.4.
- **The default is allow.** A manifest that must fail closed says `"default":
  "deny"`.
- Open: rates ("no more than 2 °C per hour") need the guard to keep history; a
  policy sector could carry a signature so a device refuses a tampered file;
  several devices under one attack are seen by one guard each, and alerting
  across them belongs to whatever collects the audit files.

---

## 12. Not in v1

Compression and content-addressed deduplication, encryption, access control
per record other than sensitivity levels (§11.5), rates and history in policies
(§11.6), signed policy sectors, typed relations between records (links carry no
label), storing the images themselves, links across sectors, adding sectors to
a running server, and keeping `application_id` and `user_version` when the
reference tool rewrites a file (§6.2, C6).

---

## 13. Conformance and stability

### 13.1 The suite

`conformance/` holds what an implementation is checked against:

- `cases/valid/*.fds`, each with a `.json` beside it: the metadata and records it
  must read as, whether a writer must reproduce it byte for byte, and whether a
  writer may rewrite it (a file from a newer minor may not be rewritten).
- `cases/invalid/*.fds`, each with a `.json` that names the category (§6.6) it
  must fail with, whether it fails at opening or at one record, and which records
  stay readable.
- `normalization.json` and `stopwords.txt`: §3.1 and §4.1 (including the search
  terms), as lines to reproduce.
- `policy.json` and `cases/audit/`: §11, as checks, template fills, redactions and
  audit chains to judge the same way (`policy.py` is the second implementation).
- `fds.py`, `normalize.py` and `run.py`: a second implementation, written from
  this text and sharing no code with the reference tool, and the script that runs
  it. `gen.py` builds every case from that implementation and by hand, and its
  output is deterministic.

The Rust tools run the same files (`cargo test -p fds --test conformance`). When
the two implementations disagree, the text of this document is wrong or unclear,
and it is the text that gets fixed.

### 13.2 Levels

| Level | What it requires |
|---|---|
| **Core** | the file format (§6), reading and writing; the record model (§2, §3, §3.1); normalisation (§4.1); stage 1 matching, steps *exact* and *normalized* (§4.2); confidence `exact` and `none` (§5) |
| **Search** | stage 1 step 3 and stage 2 (§4.2, §4.3) with the confidence rules `strong` and `weak` (§5) |
| **Links** | links, backlinks and contexts (§4.4, §4.5) |
| **Interfaces** | the command line (§7), the HTTP API (§8), MCP (§9), the change journal (§3.5, §7.2) |
| **Sectors** | §10 |
| **Guard** | the check and its number grammar (§11.1, §11.2), the guard's template filling and its refusal of what cannot be checked (§11.3), the audit chain (§11.4), the redaction of sensitive records (§11.5) |

A Core implementation that leaves out Search answers `none` when stage 1 does
not match. The Guard level is covered by `policy.json` and `cases/audit/`; the proxy itself
(§11.3's handling of lines, batches and failures) is covered by the reference
tool's tests, not by the suite.

Search is judged by its properties and not by identical rankings: BM25 and
fusion parameters are the reference implementation's, and two implementations
may order the same records differently and both conform. What is normative is
the keyword gate (§4.3 step 3), the coverage condition of stage 1 (§4.2), and
the definition of each confidence value (§5). The standard fixes what a query
may answer, not how an engine finds it.

### 13.3 Claiming conformance

An implementation may say it conforms to FDS 4.0 Core when it reads every file
in `cases/valid` as its `.json` says, rewrites the reproducible ones byte for
byte, fails every file in `cases/invalid` with the category its `.json` names,
and reproduces every line of `normalization.json`. It may add a level (Search,
Links, ...) only for what the suite covers; the suite covers Core. Claiming a
level the suite does not cover is a claim, not a result.

### 13.4 Stability

This specification is **experimental** while its version is 0.x. The format
number is 4 because three earlier layouts existed during development, not
because it has been used for years. Until this document is marked 1.0, an
incompatible change may still happen. It would be a new major version, and the
reference tool would keep reading the old one, as it reads versions 2 and 3.

From 1.0 on, the intent is that a file of major 4 stays readable by every later
tool, and that a change needing a new major comes with a migration path in the
reference tool and with new cases in the suite.
