# Changelog

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
  record contains every word of the question: 97–100% precise across five
  test domains, and no out-of-domain question reaches it.
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
  cost 2.7× the tokens, with 18 records there was no difference.
- `research/`: 70+ end-to-end checks, scale up to 100,000 records, answer
  quality in five domains, memory with several agents.
