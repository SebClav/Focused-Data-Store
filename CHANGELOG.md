# Changelog

## Unreleased

### Added

- `--read-only`, a global flag: the command line refuses every command that
  creates or modifies a database, a sector or the audit file (`init`, `add`,
  `import`, `fact`, `proc`, `context set|import`, `log`, `journal publish`,
  `policy add`, `guard`, `rm`, `embed`, `serve`, `sector` except `ls`, and
  `mcp --write`). It is a guard in this tool, not a security boundary; agents
  that need to write do it over MCP with their own sector key.
- `fact add` and `fact import` warn when a fact replaces a different answer,
  source or collection. A question is one fact per database whatever its
  collection (SPEC.md §3.1); separate knowledge belongs in a sector.
- `research/`: the scripts, the questions and the recorded results behind the
  figures below. `accuracy.py` and `verify.py` also read data from the
  project they were written in and cannot be re-run from this repository as
  they are.

### Fixed

- A negation is never a stopword (SPEC.md §4.1, revision 2026-10-29). The
  standard list contained `no`, `nor`, `not`, `cannot` and `sin`, so
  "Can users delete records?" and "Cannot users delete records?" had the same
  token set key, and the stored answer came back as `exact` for its opposite.
  The list is now 193 words in `conformance/stopwords.txt` and in the Rust
  tool, with eleven new vectors (the Python suite runs 287 checks, from 276).
- Interrogatives and modals are no longer stopwords either (SPEC.md §4.1,
  revision 2026-10-30): `can`, `should`, `how`, `who`, `when`, `where`,
  `which`, `cómo`, `quién`, `cuál` and their kin, 23 words, so the list is 170.
  "Can users delete records?" no longer returns the answer stored for
  "Should users delete records?" or "How can users delete records?" as `exact`.
  Seven new vectors; the Python suite runs 294 checks. The benchmark
  (`benchmark_v3`, S7) found this.
- Retrieval: a fact or procedure whose whole question is contained in the query
  now has its stage-2 score doubled. In the benchmark's 600-record fixture
  "cafe menu items" did not return "What are the menu items?" at all and an
  agent shipped an empty menu; it now ranks second. Calibration on 12 queries:
  11 hits, from 10. "menu prices and availability" still misses: the words
  share no term with the stored question or answer (`prices`/`price`,
  `availability`/`available`), and stage 2 has no stemming.
- `fds mcp --sectors` honoured no `--read-only`: an agent started read-only
  could still write with `fds_remember`. It now gets no memory tools.
- `fds_remember` replaced a fact a person had verified when it was given the
  same question. It now refuses; an agent's own earlier answer is still replaced.

### Changed

- The command-line tool is the full `fds-cli`; the reduced package that
  lived at the repository root is gone. The sources were reformatted with
  rustfmt and `main.rs` was split into the argument definitions and one module
  per command group.

## 0.1.0 — agents

First public release, aimed at agents.

- One-file database (`.fds`, format 4.0, **experimental**): facts, procedures,
  document fragments and image descriptions, each with its source, who verified
  it and when. The format is specified byte by byte (SPEC.md §6): a fixed
  64-byte header, records as deterministic CBOR (RFC 8949), a CRC-32 on the
  header, the metadata, the index and every record, so damage is an error and
  never a wrong answer. A newer minor version can be read by an older tool and
  is never rewritten by it. Files of versions 2 and 3 are read, and migrate the
  first time they are saved.
- A conformance suite (`conformance/`): 9 valid and 42 damaged files, test
  vectors for normalisation and identifiers, 35 checks, template fills and
  redactions for the guard, and 6 audit chains, with a second implementation in
  Python written from the spec alone. The Rust tools and the Python one read the
  same files and judge the same cases the same way.
- Query with `exact`, `strong`, `weak` or `none`. `strong` now means the first
  record contains every word of the question: 97–100% precise in the four test
  domains measured (`research/results/confidence_rules.json`), and no
  out-of-domain question reaches it. The price is coverage: `strong` is given
  to 32–36% of the direct questions, against 67–95% before.
- A change journal: agents write what they did, and why, with `fds_log` over MCP
  (or a person with `fds log`); `fds journal show|export` reads it, and
  `fds journal publish` commits it as a Markdown page to a branch of a git
  repository (default `fds`) without touching the working tree, the index or the
  branch you are on. Entries wait as *not yet reviewed* until a person checks them.
  A reviewing agent reads the journal back over MCP with the read-only tool
  `fds_journal` (timeline and detail, filters by agent, role, branch and date).
- The black box: journal entries carry a role, a status and who a step was handed
  to, and `fds journal show --timeline` prints the whole run as numbered lines
  (`#04 [reasoning] technical decision delegated → decision agent`).
- Rules an agent cannot argue with: policies a person verifies, `fds check`
  (allow or deny with the rule and its text, exit code 3 on deny, no model),
  `POST /check` and the MCP tool `fds_check`.
- `fds guard`: stands between an agent and the real tool server, checks every
  tool call against the policies, forwards what passes, refuses what does not,
  and refuses what it cannot check or cannot record. It forwards the message it
  parsed, never the line it read, and refuses batches and malformed lines.
- An audit of every decision (`audit.jsonl`), each line carrying the hash of the
  one before: `fds audit verify` finds a line that was changed, removed or
  inserted. Tamper-evident, not tamper-proof; the guard's decisions also appear
  in the timeline.
- Sensitive records: a record marked `personal`, `secret`... reaches an agent only
  if its sector is cleared for that level, otherwise as the owner's own `derived`
  text or `[protected: …]`. The replacement happens before indexing, so hidden
  words cannot be searched for.
- Matching is Unicode-aware beyond Latin: text is folded to NFC before it is
  compared, so an accent typed as a separate combining character finds the same
  word, and scripts that rely on combining marks are no longer cut in pieces.
- Links `[[…]]` and contexts with a hash: an agent that already holds a
  context confirms it in one line.
- `fds context import AGENTS.md` and `fds context export`: an AGENTS.md (or any
  Markdown) in and out of a context.
- Sectors: one database and one key per agent, shared sectors it can read but
  not write, `fds sector promote` for a person to make an agent's finding
  everyone's, live key rotation.
- MCP over stdio and over HTTP (`/mcp`), for protocol revision 2026-07-28 and
  the `initialize`-based revisions before it; `fds mcp --connect` bridges
  stdio clients to one server; `fds_remember` and `fds_forget` for an agent's
  own memory.
- Safe concurrent writes (a lock per database) and readers that re-read a file
  someone else changed.
- HTTP API and a browser manager in six languages.
- Measured with real agents (`research/agent/haiku_test/`): twelve Haiku subagents
  built parts of a site with FDS and with the whole data file in context. Same
  accuracy (35/35, nothing invented, traps included); with 508 records the file
  cost 2.7× the tokens, with 18 records there was no difference. One run per
  condition: a trend, not a statistic.
- `research/`: 70+ end-to-end checks, scale up to 100,000 records, answer
  quality in four domains, memory with several agents. The records and the
  queries were written by the same author, and the figures were measured
  before the stopword revision listed under Unreleased.
