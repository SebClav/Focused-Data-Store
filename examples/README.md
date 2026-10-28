# FDS examples

`agent_guide.fds` is a small sample database with owner-provided guidance for coding agents. It demonstrates querying records and citing their IDs and sources.

Run these commands from the repository root:

```sh
cargo run -- --read-only --db examples/agent_guide.fds --json status
cargo run -- --read-only --db examples/agent_guide.fds --json query "How should an AI coding agent use this FDS file?" --collection agent-guidance
```

Treat this file as read-only unless a human explicitly authorizes changes. It is an example database, not a substitute for project-specific review.
