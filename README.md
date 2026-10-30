# FDS

FDS (Focused Model Data Store) is a database for knowledge that AI systems
consume. One portable `.fds` file holds verified facts, procedures, document
fragments and image descriptions, each with its source and who verified it. A
query returns records and a confidence (`exact`, `strong`, `weak` or `none`);
FDS never generates text. It works next to SQL or MongoDB: those keep the
operational data, FDS keeps what agents should be told.

The status is **experimental** (format 4.0, specification 0.x).

## What is here

| Path | What it is |
|---|---|
| `fds-core/` | The `.fds` format as a Rust library: versions 2, 3 and 4, CRC-32 on every region, atomic writes |
| `fds-cli/` | The `fds` command-line tool: facts, procedures, contexts, sectors, MCP, HTTP API and browser manager, change journal, policies, guard and audit |
| `conformance/` | The format conformance suite and a second implementation in Python, written from the specification |
| `SPEC.md` | The specification: format, query semantics, sectors, policies |
| `site/how-i-work.html` | The animated page of the flow from a request to human review |
| `examples/` | A sample agent guide and the `cafe-aurora` multi-agent example |

## Build and test

Install Rust, then:

```sh
cargo build --release -p fds-cli      # target/release/fds (fds.exe on Windows)
cargo test --workspace                # Rust tests
python conformance/run.py             # the Python implementation, same files
```

## Quick start

```sh
fds init project                                   # creates project.fds
fds fact add "What is the service port?" "443" --collection project --source network-spec
fds add docs/ -c project                           # ingest a folder of documents
fds query "service port"
fds status
```

`fds <command> --help` lists the options, and `fds-cli/GUIDE.md` walks through
every command.

## Several agents

Give every agent a sector: its own database file, which it can write, plus the
shared ones it may read and cannot change.

```sh
fds sector new shared
fds sector new sales --agent --reads shared        # prints the agent's key, once
fds serve --sectors sectors.json                   # HTTP API, manager and MCP at /mcp
```

What an agent saves over MCP is marked `model:<name>` and stays in its own
sector until a person reviews it and makes it everyone's with
`fds sector promote`. The agent can also write what it did and why with
`fds_log`, and `fds journal show` reads it back. Policies a person verified
are enforced by `fds check` and by `fds guard`, which records every decision
in a hash-chained audit.

For an agent that is only given the command line, use `--read-only`: the tool
then refuses every command that creates or modifies a database, a sector or
the audit file. `FDS.md` is a ready-made instruction file for such an agent.

## What to keep in mind

- **Labels are not authentication.** `verified_by` and `source` are text in the
  file. Anyone who can write the file can change them. `--read-only` is a guard
  in this tool and keys only separate agents that go through `fds serve`; a
  process that can read the `.fds` files reads everything in them. Use file
  permissions where that matters.
- **`exact` is about the words, not the meaning.** Stage 1 compares the words of
  the question after normalisation. Negations, interrogatives and modals count
  (`not`, `who`, `how`, `can`, `should`), so questions that differ by one of them
  are different questions. Words like `what`, `is` and `the` are ignored, and a
  paraphrase that shares no word is not `exact`. See SPEC.md §4.2.
- **A question is one fact per database.** The identifier comes from the
  question alone (SPEC.md §3.1), so the same question in another collection
  replaces the fact, and `fact add` warns when it does. Separate knowledge
  belongs in separate sectors.
- **Retrieval is lexical.** Stage 2 ranks by words (BM25), with a half-weight
  match for plurals and close forms. It does not understand meaning; optional
  embeddings help, and a query that shares no word with a record can miss it.
- **No warranty, and nobody has reviewed it.** This is a personal, experimental
  tool. Where the records say `human`, the tool only reads a label: it does not
  know who wrote it.
- **The figures are early.** The numbers in `CHANGELOG.md` come from small
  test sets written by the same author as the records; the measurement scripts
  are not part of this repository.

## License

Apache License 2.0. See [LICENSE](LICENSE).
