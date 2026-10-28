# FDS

FDS is a local knowledge database for AI agents and applications. It stores project facts, procedures, and context in a portable .fds file. It works alongside SQL or MongoDB: those systems keep operational data, while FDS gives agents approved project guidance to retrieve.

This package contains the command-line core only. It does not include the graphical manager or web UI.

## Build

Install Rust, open a terminal in this folder, then run:

~~~powershell
cargo build --release
~~~

The executable is target/release/fds.exe on Windows and target/release/fds on macOS/Linux.

## Quick start

~~~powershell
cargo run -- init project
cargo run -- --db project.fds status
cargo run -- --db project.fds fact add "What is the service port?" "443" --collection project --source network-spec
cargo run -- --db project.fds --json query "service port" --collection project
cargo run -- --db project.fds ls --collection project
~~~

Use fds --help and fds <command> --help to see the commands available in this build. The .fds file can be copied and versioned with the project. Keep backups before editing it.

## Teach a coding agent to use FDS

1. Put agent_guide.fds and FDS.md in the project, or point the agent to their absolute paths.
2. Configure the agent's project instructions to read FDS.md before starting work. FDS.md is a portable instruction file; agent products may require referencing it from their own instruction mechanism.
3. Give the agent read-only access to the .fds database when it only needs project context.
4. Require the agent to query relevant facts and cite the returned record IDs and sources in its handoff.
5. Let the human review and enter any proposed knowledge updates.

Example read-only query:

~~~powershell
fds --db agent_guide.fds --json query "How should an AI coding agent use this FDS file?" --collection agent-guidance
~~~

Query labels describe retrieval matches, not whether a fact is true. Review important facts and their sources. In this build, some advanced command names are present in help but not implemented; do not assume MCP, embeddings, policy enforcement, audit history, or graphical management is available in this CLI-only package.

## License

FDS is licensed under the Apache License 2.0. See [LICENSE](LICENSE) for the full text.
