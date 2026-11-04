# FDS instructions for agents

Use the configured MCP server to retrieve context before acting.

1. List contexts with `fds_contexts`; load one with `fds_context`. Reuse its
   hash with `known_hash` to avoid loading unchanged instructions.
2. Retrieve records with `fds_query`; cite IDs and sources. Retrieval confidence
   describes matching, not factual certainty.
3. Treat `model:<name>` records as proposals awaiting review.
4. With writable memory, save useful notes through `fds_remember`. Document
   changes through `fds_log`, with reasons and file/line references.
5. Read prior decisions through `fds_journal`.
6. Leave human approval to the owner; do not mark your own work human-reviewed.

MCP without `--write` only reads. Sector writes are restricted to the agent's
own memory through this interface. Direct file access needs OS permissions.

For CLI reads: `fds --read-only --db project.fds query "question"`.
For authorized logging: `fds --db project.fds log "Summary" --by AGENT --why "Reason" --file path:10-20`.

See `docs/GUIDE.md` and `fds --help` for supported commands.
