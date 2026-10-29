# Real agents on a real task: FDS against reading the whole file

Twelve Haiku subagents (not a simulation) built parts of a business website
from the café data in `examples/cafe-aurora`. Each did one of three tasks, once
with the data in an FDS database and once with the whole data file read into its
context:

| Task | The agent writes | How it is scored (`check.py`) |
|---|---|---|
| textos | an HTML section with hours, address, phone and two price lists | each of 22 facts present exactly; any price or time the data does not have counts as invented |
| imagenes | four `<img>` tags | `src` and `alt` exactly as stored (8 checks); any other `src` is invented |
| marca | CSS variables and fonts | three colours and two fonts (5 checks); any other hex colour is invented |

The condition **A** agent gets a terminal command (`fds --db ... query "..."`) and
is told not to open the data file. The condition **B** agent gets the file and is
told to read all of it. The prompts are otherwise identical.

Two rounds: the café's 18 records (5.5 KB), and the same 18 hidden among 490 others
(239 KB, built by `gen_big.py`), ten of them traps: another site's opening hours,
price lists, brand colours and photos.

## Results

Tokens are the subagent's `total_tokens` as reported by the harness. About 56,000
of every number is the agent's own fixed overhead (instructions and tool
definitions), which is why the small round shows no difference.

| | records | A: FDS | B: whole file | B / A | correct (A, B) | invented |
|---|---|---|---|---|---|---|
| small | 18 | 170,751 | 172,198 | 1.0× | 35/35, 35/35 | 0, 0 |
| large | 508 | 176,174 | 477,946 | **2.7×** | 35/35, 35/35 | 0, 0 |

(Sum over the three tasks. Per-task numbers are in `results.json`; the files the
agents wrote are in `outputs/`, and `python check.py outputs/small` scores them.)

What this says:

- **The cost of A does not grow with the data.** 170,751 tokens for 18 records,
  176,174 for 508. The cost of B did: each of its agents spent about 100,000
  tokens on the data of the large file (7 to 16 tool calls).
- **Below a few thousand tokens of data there is no saving.** The file fits in
  the prompt and A makes more calls (21 against 9) and takes longer (65 s against
  38 s in total). FDS does not pay for itself on a small file.
- **Accuracy did not differ.** All 12 runs got every fact and invented nothing,
  including with the traps. This is not evidence that FDS improves accuracy on
  this task; it is evidence that it does not cost any.

## What this does not say

- One run per cell, one small model, one kind of task. It is a measurement, not
  a statistical result.
- The data file in B was read in full because the agent was told to. An agent
  allowed to `grep` it would spend far less, and would have to know what to
  search for. That is a different, fair comparison and it was not run.
- The filler records are synthetic. The traps are written to collide on
  purpose; real data collides less, or in other ways.
- The agents were told to use `fds` from the terminal. Nothing here tests
  whether an agent chooses to consult FDS on its own.
- Some queries came back `weak`, not `exact`, with the right record first (one
  agent reported it for the pastry price list). The agents read the confidence
  correctly and used the record. The transcripts of most agents were not
  recoverable, so why is not established; one run did add words to the question
  (`carta de pastelería precios`). Separately, checked by hand: matching does not
  yet ignore accents, so `horario de atencion` is `weak` where `horario de
  atención` is `exact`.

## Reproduce

```
python gen_big.py            # writes negocio_grande.json next to it
fds --db big.fds init big && fds --db big.fds import negocio_grande.json
```

Then give an agent the task and either the database command or the file, write
its output into a folder, and `python check.py <folder>`. `check.py` expects
files named `textos_A.html`, `textos_B.html`, `imagenes_A.html`, ... .
