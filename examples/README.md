# FDS examples

## `agent_guide.fds`

A small sample database with owner-approved guidance for coding agents. It
shows how to query records and cite their IDs and sources.

Build the tool once (`cargo build --release -p fds-cli`), then, from the
repository root:

```sh
fds --read-only --db examples/agent_guide.fds --json status
fds --read-only --db examples/agent_guide.fds --json query "How should an AI coding agent use this FDS file?" --collection agent-guidance
```

Treat this file as read-only unless a human explicitly authorizes changes.

**The records predate the current tool.** The guide still names the file
`agent_guide.fds`, says that `embed`, `mcp`, `sector`, `log`, `journal`,
`policy`, `check`, `guard` and `audit` are not implemented, and says that new
facts are automatically marked as verified by a human. None of that holds any
more: all those commands exist, `--read-only` is available, and an agent that
writes over MCP is marked `model:<name>`. The root `FDS.md` has the current
instructions. The records are left as they were because they carry the owner's
approval; rewriting them is the owner's decision.

## `cafe-aurora/`

Four small agents build the home page of a (fictional) café. None of them has
the business's data pasted into its prompt; all of them query one FDS database.
`simular_agentes.py` runs the queries each agent would make over MCP, read-only
and with no model, and measures what each receives. The example and its README
are in Spanish. From the repository root, after building the tool:

```sh
python examples/cafe-aurora/simular_agentes.py
```

On Windows, set `PYTHONIOENCODING=utf-8` first, or the console cannot print the
script's output.
