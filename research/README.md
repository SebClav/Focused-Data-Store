# research/

> **In this repository.** The recorded results (`results/`), the domain
> questions (`domains/`) and the scripts are here so that the figures in
> `CHANGELOG.md` can be read and checked. `accuracy.py` and `verify.py` also
> read a `winops/` data set from the project they were written in, which is not
> part of this repository, so those two cannot be re-run as they are. The
> results were measured before the stopword revision of 2026-10-29 (SPEC.md
> §4.1) and `results/accuracy.json` covers four domains: `electronics`,
> `linux`, `soporte` and `winops`.

Evidence that `fds` works, measured on the real release binary. Nothing is
mocked; every script drives the CLI, the HTTP API or the MCP server the way a
person or an agent would.

| Script | What it answers | Output |
|---|---|---|
| `verify.py [--images]` | Does every feature do what the docs say? CLI, import, robustness (corrupt files, Unicode, v2→v3, concurrent writers, killed writes), HTTP security, MCP. | `results/verify.json` |
| `scale.py [--sizes 1000,10000,100000] [--vectors 10000]` | How build time, file size, query latency, write time and memory grow. | `results/scale.json` |
| `accuracy.py [--no-vectors]` | How often the right record comes first, in four domains (`domains/*.json` plus `winops/`), keyword-only and hybrid; how often a confident answer is wrong; how out-of-domain questions are handled. | `results/accuracy.json` |
| `confidence_rules.py` | Replays the hybrid answers under alternative rules for `strong`. Run after `accuracy.py`. | `results/confidence_rules.json` |
| `sectors_ram.py [--agents 5] [--shared 10000]` | Memory and latency of N agents sharing one base: a `fds mcp` process each, against one `fds serve --sectors` and data-free bridges. | `results/sectors_ram.json` |
| `agent/run.py` | A real cloud agent (Claude Haiku via the Claude Code CLI) answering with no memory, the whole base in its prompt, FDS as an MCP tool, or FDS first. Needs `claude auth login`. | `results/agent_runs.jsonl` |

Build first: `cargo build --release -p fds-cli`. Vector checks need Ollama with
`all-minilm`; the image check needs `qwen2.5vl:3b`. On Windows, run with
`PYTHONIOENCODING=utf-8`.

## Reading the accuracy numbers

Queries come in three tiers, so the average does not hide where FDS breaks:
1 literal (the record's own words), 2 paraphrase, 3 indirect (describes a
situation, not the task). Out-of-domain questions are *near* (the domain's
vocabulary, no record for them) or *far*.

The records and the queries were written by the same author. The tiers limit
that bias but do not remove it; a query set written by people who never saw
the records is the next step.
