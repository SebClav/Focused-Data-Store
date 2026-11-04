# Examples

`agent_guide.fds` is an older owner-approved guidance database. Some command
availability claims in its stored records are outdated; use the root FDS.md
for current instructions. Treat the database as read-only.

`cafe-aurora/` contains fictional cafe facts, a context document and a sample
FDS database. Query it using local MCP or the CLI. No Python is required.

```sh
fds --read-only --db examples/cafe-aurora/cafe-aurora.fds query "horario"
fds --read-only --db examples/cafe-aurora/cafe-aurora.fds mcp
```
