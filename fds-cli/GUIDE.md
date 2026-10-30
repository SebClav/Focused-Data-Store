# fds

The reference command-line tool for FDS databases. The format and query
semantics are defined in [SPEC.md](../SPEC.md).

A FDS database is one `.fds` file holding three kinds of records:

- **facts** — a question and its verified answer, served without any model
- **procedures** — an intent and its steps (fields, selectors, actions)
- **knowledge** — fragments of your documents and code, and descriptions of
  your images

Any record can also have a free **type** of your own (`command`, `receta`,
anything), **attributes**, and **links** to other records written
`[[like this]]`, the way Obsidian links notes. A **context** is a named set of
instructions plus the records it links to, loaded by an agent in one call and
not re-read while it has not changed.

A query returns records with a confidence (`exact`, `strong`, `weak`, `none`)
and provenance. `fds` never generates text: what to do with the records is up
to the caller.

## Build

```
cargo build --release -p fds-cli
```

The binary is `target/release/fds` (`fds.exe` on Windows).

On Windows, Rust needs a linker: either Visual Studio Build Tools with the C++
workload, or the GNU toolchain plus MinGW:

```
rustup toolchain install stable-x86_64-pc-windows-gnu
winget install --id BrechtSanders.WinLibs.POSIX.MSVCRT -e
cargo +stable-x86_64-pc-windows-gnu build --release -p fds-cli
```

## Quickstart

```
fds init electronics                       # creates electronics.fds
fds add handbook/ -c electronics           # ingest a folder of docs
fds fact add "what is the lm317 reference voltage" "1.25 V"
fds proc add form_a.json                   # compiled procedures
fds status

fds query lm317 reference voltage          # no quotes needed
fds query --json "how do I add a new customer"
```

Everything above works offline, with no model. For meaning-based matching,
add embeddings (needs [Ollama](https://ollama.com) with `all-minilm` pulled):

```
fds embed
```

## Commands

| Command | Does |
|---|---|
| `fds init [name]` | create an empty database |
| `fds status` | counts, collections, embeddings, size |
| `fds add <path>... [-c C] [--lang es]` | ingest files, folders and images; re-adding a path replaces it |
| `fds fact add <q> <a> [-c C] [--source S]` | add or replace a verified fact |
| `fds fact import <file.json> [-c C]` | import many facts |
| `fds proc add <file.json> [-c C]` | add or replace procedures |
| `fds context set <name> --instructions T [--link X]...` | create or replace a context |
| `fds context ls` / `fds context show <name> [--known HASH]` | list contexts / load one |
| `fds context import AGENTS.md` / `fds context export <name>` | an AGENTS.md (or any Markdown) in and out of a context |
| `fds ls [--kind K] [--type T] [-c C]` | list records |
| `fds show <id>` | one record with its provenance |
| `fds rm <id>` / `fds rm --source <path>` | remove records |
| `fds embed [--model M] [--all]` | compute embeddings |
| `fds query <text> [-c C] [--budget N] [--links] [--verified-by human]` | query; `--links` adds what the results link to, `--verified-by` keeps only what those parties verified |
| `fds import <file> [-c C] [--dry-run] [--skip-errors]` | import a CSV (Excel's `;` and accents included), TSV, JSON or JSON Lines |
| `fds serve [--port 7070] [--open]` | HTTP API, the browser manager (below) and MCP at `/mcp` |
| `fds serve --sectors sectors.json` | every sector at once, for several agents (below) |
| `fds mcp [--write] [--as NAME]` | MCP over stdio; `--write` lets the agent remember |
| `fds mcp --connect URL` | stdio bridge to a running server, key in `FDS_KEY` |
| `fds sector new/ls/key/promote` | one database per agent, plus shared ones |

Global flags: `--db <file>` to pick a database (default: the only `*.fds` in
the current directory), `--json` for machine-readable output.

Exit status: `0` success, `1` error, `2` query found nothing.

## Images

```
fds add fotos/ --lang es
```

describes every image with a local vision model (`qwen2.5vl:3b` through
Ollama by default; `--vision-model` or `FDS_VISION_MODEL` to change it) and
stores the description, not the image. The model copies any visible text
verbatim first — codes, model names, error messages — then describes the
picture, so a search for `13.B2.D1` finds the photo of the printer showing
it. Each record says `verified_by: model:<name>` until you check it, and an
image added again unchanged is not described again.

## Contexts

```
fds context set "Network support" -c support   --instructions "Answer in Spanish, briefly. [[how do I reboot the router]]"   --link "router admin ip"
fds context show "Network support"
fds context show "Network support" --known 8f0d273565fd7f4f   # one line if unchanged
```

## What the agents did: the change journal

When agents write code, the people who read it later were not there. Each agent
can write down what it did, and you read it as a page:

```
fds log "Add a rate limit to /login" --why "Five failures lock the address for ten minutes." --file src/auth.rs --by backend
fds journal show                          # newest first; add --md for Markdown
fds journal export --out JOURNAL.md       # a page to read anywhere
fds journal publish --repo ../myproject   # commit it to the branch `fds` of that repository
git -C ../myproject push origin fds       # share it, when you decide to
```

Agents connected over MCP with `--write` get a tool, `fds_log`, for the same
thing; any agent, with or without `--write`, can read the journal back with the
read-only `fds_journal` (timeline plus detail; filters `limit`, `agent`, `role`,
`branch`, `since`), which is how a reviewing agent checks the work. Their entries
show as *not yet reviewed* until you check them. `publish`
never changes your files, your index or the branch you are on.

### The black box

Give each step a role, a status and, when it hands work on, who it went to, and
read the whole run as a numbered timeline:

```
fds log "problem recognized"            --role me
fds log "consultation sent"             --role me        --to "reasoning agent"
fds log "technical decision delegated"  --role reasoning --to "decision agent" --by claude-r
fds log "task 1"                        --role execution --status running --by claude-e1
fds journal show --timeline
```

```
#01 [me] problem recognized
#02 [me] consultation sent → reasoning agent
#03 [reasoning] technical decision delegated → decision agent
#04 [execution] task 1 · running
```

The agent writes its own entries with `fds_log`; they are as honest as the agent.
What the guard (next section) records is not up to the agent.

## Rules an agent cannot argue with: policies, the guard and the audit

A policy is a rule you verified. Checking one is a comparison, not a model, so it
cannot be talked out of anything.

```
fds policy add "Below -20 C the stock is damaged." --subject fridge.chamber3.setpoint --action set \
    --rule range --min -20 --max -16 --unit C \
    --tool set_temperature --tool-subject "fridge.{chamber}.setpoint" --tool-value "{celsius}"
fds policy add "Nobody drops production." --subject "db.prod.*" --action drop --rule deny \
    --tool run_sql_drop --tool-subject "db.prod.{table}"

fds check --subject fridge.chamber3.setpoint --action set --value -40     # deny, exit code 3
fds check --subject fridge.chamber3.setpoint --action set --value -18     # allow, exit code 0
```

The `--tool` flags say how a tool call becomes a check. The **guard** uses them: it
stands between the agent and the real tool server, checks every `tools/call`,
forwards what passes, refuses what does not, and records each decision.

```
# in the agent's MCP configuration, instead of the real server:
fds guard --policies sectors.json --agent claude-r -- node real-tool-server.js

fds audit show                    # what was decided, newest first
fds audit verify                  # no line changed, removed or inserted; prints the last hash
fds journal show --timeline --sectors sectors.json    # the guard's decisions and the journal, together
```

Put the real server's credentials in the **guard's** environment, not the
agent's: the guard has to be the only way to the tool. An agent that has its own
path to the tool is not guarded.

Policies count only if a person verified them (`fds policy add` does). Add
`"default": "deny"` to the manifest for subjects that must never fail open. Keep
the hash `audit verify` prints somewhere the agent cannot write: a chain cannot
show that its last lines were cut.

## Personal data an agent should not read

Mark a record with a sensitivity level and an optional `derived` text, in an
import file:

```json
{ "kind": "fact", "question": "address of Juan Perez", "answer": "Calle Secreta 99, Miraflores",
  "attrs": { "sensitivity": "personal", "derived": "The customer lives in Lima" } }
```

An agent whose sector is not cleared for `personal` gets the `derived` text, or
`[protected: personal]`, and cannot find the record by searching for its hidden
words. Clear a sector with `fds sector new accounts --agent --reads customers
--clearance personal`, or edit `clearance` in `sectors.json`. For one database,
`fds mcp --clearance personal`. The manager, the command line and the export are
yours: they show everything.

## The manager

```
fds serve --open
```

opens a database manager in your browser at `http://127.0.0.1:7070` — the
same idea as pgAdmin or SQL Server Management Studio, for `.fds` files. It is
served by the `fds` binary itself; there is nothing else to install, and it
loads nothing from the internet (its font ships inside the binary).

- **Explorer** — the database, its record kinds and its collections, with counts.
- **Records** — browse, search and sort; select a record for its full content
  and provenance; delete it.
- **Query** — run a query and see the confidence, every record, and the raw
  `bm25` and `cosine` behind each one. `Ctrl+Enter` runs.
- **Overview** — one tile per question: what is in the database, how much of
  it a model wrote and still waits for review (one click filters to it),
  embedding coverage, contexts, types, collections, the file, and the MCP and
  HTTP snippets to connect an agent or a script.
- **Command palette** — `Ctrl+K` (`⌘K`): any action, context or record by
  name, from the keyboard. `↑`/`↓` walk the records grid.
- **Day and night** — follows the system until you pick one; the pick is kept.
- **New fact / New procedure / New context** — write one record by hand.
- **Edit** — change a record's text, type or links; `[[name]]` in the text
  links it. Saving an edit marks the record verified by a person.
- **Graph** — every record and link drawn as a graph; drag, zoom, click a node
  to open it.
- **Import** — load many at once. Drop a CSV, TSV or JSON file, a folder of
  documents or images, or paste rows straight from Excel. Columns are matched to FDS's
  fields by name, in English or Spanish; a CSV saved by Excel with `;` and
  Windows-1252 accents is read correctly. Before anything is written you see
  every row marked *new*, *replaces*, *unchanged*, *duplicate* or *error*, and
  a batch with errors is refused whole unless you choose to leave those rows
  out. Rows that did not change are not rewritten, so they keep their
  verification date.
- **Export** — CSV for a spreadsheet or JSON for everything, in the same
  shape Import reads: edit in Excel, import back, and only what you changed
  changes. Drag a file anywhere onto the manager to start an import.

It edits the real file. Writes read the file first, so a `fds fact add` run
in another terminal while the manager is open is kept, not overwritten.

The server listens on loopback only and refuses writes without a per-session
token, which the manager page carries for you; the terminal prints it if a
script needs to write. `/query` and `/status` need no token. See SPEC.md §8.2
for the routes and the reasons.

## Procedure files

```json
[
  {
    "intent": "register a new customer",
    "title": "Customer registration form",
    "steps": [
      { "action": "open",  "url": "/clientes/nuevo" },
      { "action": "fill",  "selector": "#cli_nombre", "field": "name" },
      { "action": "click", "selector": "button[data-qa='guardar']" }
    ]
  }
]
```

`intent` is what queries are matched against. Any other keys are kept in the
body and returned as-is; `fds` stores procedures, it never runs them.

## Several agents, each with its own memory

Give every agent a sector: its own database file, which it can write, plus the
shared ones it may read and cannot change.

```
fds sector new shared                          # the knowledge everyone reads
fds sector new sales --agent --reads shared    # prints the agent's key, once
fds sector new ops   --agent --reads shared
fds serve --sectors sectors.json
```

Each agent connects to `http://127.0.0.1:7070/mcp` with its key as
`Authorization: Bearer <key>`, or, if its client only starts stdio servers,
through a bridge:

```json
{ "mcpServers": { "fds": {
    "command": "fds", "args": ["mcp", "--connect", "http://127.0.0.1:7070/mcp"],
    "env": { "FDS_KEY": "fdsk_…" } } } }
```

- The agent queries its own sector and `shared` together; every record says
  which sector it came from.
- `fds_remember` saves into its own sector only, marked `model:sales`; another
  agent never sees it.
- A person reviews it and, if it is right, makes it everyone's:
  `fds sector promote <id> --from sales --to shared`.
- `fds sector key sales` replaces a key; the old one stops working at once.

One server holds every file once, however many agents connect: five agents over
a shared base of 10,000 records took 110 MB this way, against 300 MB with one
`fds mcp` process each. Each sector keeps its own manager at
`http://127.0.0.1:7070/s/<name>/`.

Writes are safe from any number of processes: every writer takes a lock on
`<file>.lock`, and readers notice a new file and re-read it.

## Using it from an MCP client

```json
{
  "mcpServers": {
    "fds": {
      "command": "/path/to/fds",
      "args": ["--db", "/path/to/electronics.fds", "mcp"]
    }
  }
}
```
