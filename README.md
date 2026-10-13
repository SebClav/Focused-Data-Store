# DMD: Deterministic Mask Database

**The memory and rules for AI.**

DMD stores verified facts, procedures, documents and image descriptions in a single file, each with its source and who verified it. A query returns records
and a confidence level (`exact`, `strong`, `weak` or `none`) and never generates text.

For example, in the use case of AI agents, several agents can share one database: each gets its own sector, its own key and its own memory, and can read the shared sectors without being able to change them (permissions).

**DMD query example**:
```
$ dmd query "flush the dns cache"
confidence: exact

[procedure] f921f7d0  flush the dns cache
    { "steps": [ { "action": "run", "command": "ipconfig /flushdns", "shell": "cmd" } ] }
    — source: winops/commands.json · verified: username-ID 2026-09-23 · match: exact (1)
```

## Why

- **Small models' availability and usability.** DMD supplies the facts and answers directly
  when the confidence level is high enough to tell the model when it is certain, so a small model only has to phrase, extract or talk. On a small test corpus, a 0.5B model's hallucination rate fell from 92% to 21%.
- **Agents stop paying a tariff for re-reading the context.** A query returns tens of tokens instead of the whole knowledge base. A shared context that has not changed costs a single line to confirm.
- **It tells the model when it does not know about a query.** A query with no shared keyword answers `none` instead of the nearest guess.
- **Every answer carries its provenance or origin**, and what a model wrote stays marked as such until a real person reviews it, and its review mark stays on the document.
- **Segmented like a microkernel.** Each agent gets its own sector: it writes only there, reads the shared sectors it is given, and cannot see another agent's memory.


## Quick start

```
cargo build --release -p dmd          # binary: target/release/dmd

dmd init knowledge
dmd fact add "router admin address" "http://192.168.0.1"
dmd add docs/                         # Markdown, text, code, images
dmd query "router admin address"
dmd serve --open                      # HTTP API and the browser manager
```

Connect an agent over MCP:

```json
{ "mcpServers": { "dmd": { "command": "dmd", "args": ["--db", "knowledge.fds", "mcp", "--write"] } } }
```

### Several agents, each with its own memory

```
dmd sector new shared
dmd sector new frontend --agent --reads shared    # prints the agent's key once
dmd sector new backend  --agent --reads shared
dmd serve --sectors sectors.json                  # one server, one /mcp for all
```

Each agent connects to `http://127.0.0.1:7070/mcp` with its key, or through
`dmd mcp --connect <url>` with the key in `DMD_KEY`. A person promotes what an
agent learned into the shared sector with `dmd sector promote`.

## Documentation

- [dmd/GUIDE.md](dmd/GUIDE.md): every command, the manager, images,
  contexts, import and export
- [SPEC.md](SPEC.md): the file format, query semantics, HTTP API, MCP and
  sectors, the contract any other implementation can follow
- [research/](research/README.md): the measurements behind every claim here,
  and the scripts to repeat them

## Status

Version 0.1 targets agents: sectors, memory tools, contexts, MCP over stdio and HTTP.



## Credits and license

Design, specification and direction: **Gabriel Sebastian Clavijo Alessandri**.


Implemented with the assistance of Claude (Anthropic) for language guidance, code learning and architecture research. 
Commits and PRs made with it carry an `Assisted-by: Claude` trailer.

Licensed under the [Apache License 2.0](LICENSE). The VT323 font bundled in the
manager is under the SIL Open Font License; see [LICENSING.md](LICENSING.md).
Contributions are welcome.


### `Attention is just your last resource`