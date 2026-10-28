# FDS agent instructions

Before coding, read `examples/agent_guide.fds` through the FDS CLI and follow the project rules below.

## Read project context

Use the database explicitly and query only what the task needs:

~~~sh
fds --read-only --db examples/agent_guide.fds --json status
fds --read-only --db examples/agent_guide.fds --json query "relevant task terms" --collection agent-guidance --budget 400
fds --read-only --db examples/agent_guide.fds --json show RECORD_ID
~~~

Cite the FDS record IDs and source values that support project-specific decisions. If no record supports a claim, say the information is missing and ask the human; do not guess. Strong and weak are lexical retrieval labels, not truth scores.

## Do not write approved knowledge

Treat the .fds file as read-only unless the human explicitly authorizes a database write. Do not use fact add, fact import, proc add, context set, context import, add, import, or rm without that authorization. This build automatically labels new facts as verified_by=human; never use that behavior to store an agent-generated claim. Put suggestions under “Proposed FDS updates” in your handoff for human review.

Use `fds --read-only --db path/to/project.fds ...` for agent queries. This CLI option blocks its own write commands; it is not an operating-system security boundary. The host must also enforce file permissions when other processes must not modify the database.

## Required handoff

Report task status; a concise summary; record IDs and sources consulted; exact files changed; implementation decisions and rationale; checks actually run (or state “not run”); unresolved risks and assumptions; and proposed FDS updates awaiting human review. Never claim tests or queries that you did not perform.
