# Cafe Aurora: shared context for agents

This is a fictional cafe. `negocio.json` holds sample facts, `brief.md` holds
shared instructions and `cafe-aurora.fds` is the example database. Image records
contain descriptions and file references; this basic CLI does not generate or
analyze images.

From the repository root:

```sh
fds --read-only --db examples/cafe-aurora/cafe-aurora.fds query "horario de atencion"
fds --read-only --db examples/cafe-aurora/cafe-aurora.fds mcp
```

To rebuild in a separate working folder, copy negocio.json and brief.md there:

```sh
fds init cafe-aurora
fds --db cafe-aurora.fds fact import negocio.json
fds --db cafe-aurora.fds context import brief.md --name "Sitio web de Cafe Aurora"
```

Agents can retrieve shared instructions and facts, save their own notes in
separate sectors and leave decisions in a journal for a person to review.
See the root FDS.md and docs/GUIDE.md for current workflows.
