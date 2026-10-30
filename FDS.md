# FDS agent instructions

Before coding, read the project's FDS file through the `fds` command and follow
the rules below. Replace `project.fds` with the file your host gives you.

## Read project context

Use the file explicitly, always with `--read-only`, and ask only for what the
task needs:

~~~sh
fds --read-only --db project.fds --json status
fds --read-only --db project.fds --json query "relevant task terms" --budget 400
fds --read-only --db project.fds --json show RECORD_ID
~~~

Cite the record IDs and the `source` values that support project-specific
decisions. If no record supports a claim, say the information is missing and
ask the human; do not guess.

## Know what a result is worth

- `exact`, `strong` and `weak` describe how the words matched. They are not
  truth scores, and none of them says a person reviewed the record.
- `exact` compares words, not meaning. Questions that differ only by a question
  word or an auxiliary (`who`, `how`, `can`, `should`) share an answer; read the
  record's own question before you rely on it.
- Look at `verified_by`. `human` means a person at a terminal or in the manager
  saved or checked it. `model:<name>` means an agent wrote it and nobody has
  reviewed it yet; say so in your handoff instead of presenting it as approved.
- `none` means no record matched. Report the gap.

## Do not write approved knowledge

Treat the `.fds` file as read-only. `--read-only` makes `fds` refuse `fact`,
`proc`, `context set|import`, `add`, `import`, `log`, `rm`, `embed`, `policy
add`, `sector` changes and `serve`, but it is a guard in the tool, not access
control: the host must also give you a read-only file or an MCP connection
that has no write tools.

If the host gave you a sector and a key over MCP, write only there:
`fds_remember` saves into your own sector, marked `model:<your name>`, and a
person decides whether to promote it. Put every other suggestion under
“Proposed FDS updates” in your handoff for human review. Never run a writing
command on a person's behalf, and never mark a record verified.

## Required handoff

Report task status; a concise summary; record IDs and sources consulted (and
which of them were `model:` and unreviewed); exact files changed;
implementation decisions and rationale; checks actually run (or state “not
run”); unresolved risks and assumptions; and proposed FDS updates awaiting
human review. Never claim tests or queries that you did not perform.
