# FDS guide

A Rust CLI and local MCP server for context, memory and human review.

## Records

```sh
fds init project
fds --db project.fds fact add "What port?" "443" --source project-spec
fds --db project.fds add docs/
fds --db project.fds import facts.json
fds --db project.fds ls
fds --db project.fds show RECORD_ID
fds --read-only --db project.fds --json query "port"
```

`import` and `fact import` read JSON facts. `proc add` reads JSON procedures.
`add` splits text, Markdown and source code; unsupported binary files are skipped.

## Context

```sh
fds --db project.fds context import AGENTS.md --name project-rules
fds --db project.fds context ls
fds --db project.fds context show project-rules
```

The MCP equivalents are `fds_contexts` and `fds_context`. Pass `known_hash`
when you already hold a context to receive a short unchanged response.

## MCP and agent memory

```sh
fds --db project.fds mcp
fds --db project.fds mcp --write --as backend
```

Configure your MCP host using absolute paths. The first command only reads.
The second allows memory and logging. `fds_remember` writes `model:backend`
records and refuses to overwrite person-verified facts.

## Several agents

```sh
fds sector new shared
fds sector new backend --agent --reads shared
fds mcp --sectors sectors.json --as backend
fds --read-only mcp --sectors sectors.json --as backend
```

A sector is a database file. MCP writes only the agent's sector; shared ones
are read-only through that interface. OS permissions govern direct file access.

## Journal and review

```sh
fds --db project.fds log "Updated validation" --by backend --why "Reject duplicates" --file backend/server.js:42-58
fds --db project.fds journal show
fds --db project.fds journal export
fds --db project.fds review --list
fds --db project.fds review
fds --db project.fds journal review RECORD_ID
```

`review` opens flagged lines in your editor; `--list` only lists them. Agents
log proposals; a person reviews them. Labels are claims, not authenticated
identities. Use `sector promote --help` to promote a record across sectors.

## Limits and checks

Retrieval is lexical. `exact`, `strong`, `weak` and `none` describe matching,
not probabilities of truth. FDS never generates answers. Run
`cargo test --workspace` to test format, retrieval, MCP and review behavior.
The specification retains descriptions of older profiles; this basic CLI
supports the commands shown by `fds --help`, with local MCP only.
