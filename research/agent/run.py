"""FDS as live memory for a cloud agent: what it saves, and what it costs in accuracy.

A real agent (Claude Haiku through the Claude Code CLI, headless) answers the
same requests four ways, and every run reports its own exact token usage:

  none     no memory: the model alone
  stuff    the whole knowledge base pasted into the system prompt (what most
           teams do first)
  tool     the agent has FDS as an MCP tool and decides when and what to ask
  cascade  the harness asks FDS first; when FDS is sure (exact, or strong on
           keywords) it answers with zero model tokens; otherwise ONE model
           call receives the top records and no tools

Knowledge is split so the result is honest: `soporte` and `runbook` are
private (no model can know them), `linux` and `electronics` are public (a big
model already knows most of it, so memory should add little there).

Correctness is judged by a second cheap model call against the stored answer;
out-of-domain requests are judged as ABSTAIN (says it does not have it) or
INVENT (asserts specifics). Every run is appended to
research/results/agent_runs.jsonl, so an interrupted run resumes.

    python research/agent/run.py --dry-run          # FDS side only, no model, no login
    python research/agent/run.py --conds none,stuff,tool,cascade --cap 6
    python research/agent/run.py --session 10 --conds stuff,tool --cap 3
"""

import argparse, json, os, random, shutil, subprocess, sys, threading, time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "research"))
from accuracy import load_domains, norm  # noqa: E402

FDS = ROOT / "target" / "release" / ("fds.exe" if os.name == "nt" else "fds")
CLAUDE = shutil.which("claude")
WORK = ROOT / "research" / "work" / "agent"
OUT = ROOT / "research" / "results"
RUNS = OUT / "agent_runs.jsonl"
KB_DOMAINS = ["soporte", "runbook", "linux", "electronics"]
PRIVATE = {"soporte", "runbook"}
PER_TIER = {"soporte": 6, "runbook": 6, "linux": 2, "electronics": 2}

BASE = ("Eres el asistente de operaciones de una empresa peruana de logística y comercio en línea. "
        "Respondes preguntas de empleados y clientes, en el idioma de la pregunta, de forma breve y exacta. "
        "Si no tienes la información, dilo con claridad; no la inventes.")
PROMPTS = {
    "none": BASE + " No tienes acceso a ninguna base de conocimiento: responde solo con lo que sepas con certeza.",
    "stuff": BASE + " Esta es tu base de conocimiento interna; responde con ella.\n\nBASE DE CONOCIMIENTO\n{kb}",
    "tool": BASE + " Tienes la herramienta de memoria fds_query. Antes de responder cualquier pregunta sobre la empresa, "
                   "sus procedimientos, comandos o políticas, consúltala. Si devuelve confianza none o nada relevante, "
                   "puedes reformular la consulta una vez; si sigue sin haber nada, di que no tienes la información.",
    "cascade": BASE + " Se te entregan REGISTROS de la base de conocimiento recuperados para esta pregunta. "
                      "Responde solo con ellos; si no hay registros o no responden a la pregunta, di que no lo sabes.",
}
JUDGE_SYSTEM = "Eres un evaluador estricto. Respondes con una sola palabra."
LOCK = threading.Lock()


# ── knowledge base ─────────────────────────────────────────────────────────
def kb_line(r):
    if r["kind"] == "procedure":
        cmd = r["steps"][0]["command"]
        return f"- {r['intent']}: `{cmd}`"
    return f"- {r['question']}: {r['answer']}"


def reference(r):
    return r["steps"][0]["command"] if r["kind"] == "procedure" else r["answer"]


def build_kb(pad=0):
    """One database holding every domain, one collection each. `pad` adds
    synthetic records so the base can grow past what fits in a prompt."""
    doms = {d["name"]: d for d in load_domains()}
    WORK.mkdir(parents=True, exist_ok=True)
    db = WORK / f"kb{pad}.fds"
    for p in (db, db.with_suffix(".fds.tmp")):
        if p.exists():
            p.unlink()
    subprocess.run([str(FDS), "--db", str(db), "init", "kb"], check=True, capture_output=True)
    lines = []
    for name in KB_DOMAINS:
        rows = WORK / f"{name}.jsonl"
        rows.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in doms[name]["records"]), encoding="utf-8")
        p = subprocess.run([str(FDS), "--db", str(db), "import", str(rows), "-c", name], capture_output=True, text=True, encoding="utf-8")
        assert p.returncode == 0, p.stderr
        lines += [kb_line(r) for r in doms[name]["records"]]
    if pad:
        rng = random.Random(11)
        vocab = ("red servidor cliente factura envío pedido almacén ruta conductor reporte cierre respaldo certificado "
                 "acceso credencial política sistema base datos tarifa contenedor aduana entrega retraso incidente "
                 "plantilla usuario permiso auditoría inventario devolución pago comprobante turno planilla").split()
        rows = WORK / f"pad{pad}.jsonl"
        pads = []
        for i in range(pad):
            q = " ".join(rng.choices(vocab, k=rng.randint(4, 7))) + f" {i}"
            a = " ".join(rng.choices(vocab, k=rng.randint(18, 40)))
            pads.append({"kind": "fact", "question": q, "answer": a})
        rows.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in pads), encoding="utf-8")
        p = subprocess.run([str(FDS), "--db", str(db), "import", str(rows), "-c", "pad"], capture_output=True, text=True, encoding="utf-8")
        assert p.returncode == 0, p.stderr
        lines += [kb_line(r) for r in pads]
    return db, "\n".join(lines), doms


def pick_tasks(doms):
    rng = random.Random(3)
    tasks = []
    for name in KB_DOMAINS:
        d = doms[name]
        by_key = {norm(r.get("question") or r.get("intent")): r for r in d["records"]}
        for tier in (1, 2, 3):
            pool = [q for q in d["queries"] if q["tier"] == tier]
            for q in rng.sample(pool, min(PER_TIER[name], len(pool))):
                r = by_key[q["gold"]]
                tasks.append({"id": f"{name}-t{tier}-{len(tasks)}", "domain": name, "tier": tier, "q": q["q"],
                              "gold": q["gold"], "ref": reference(r), "kind": "answer"})
    for name, kind, n in (("runbook", "near", 4), ("soporte", "near", 3), ("runbook", "far", 3)):
        pool = [o for o in doms[name]["ood"] if o["kind"] == kind]
        for o in rng.sample(pool, min(n, len(pool))):
            tasks.append({"id": f"{name}-ood-{kind}-{len(tasks)}", "domain": name, "tier": kind, "q": o["q"],
                          "gold": None, "ref": None, "kind": "ood"})
    return tasks


# ── FDS side ───────────────────────────────────────────────────────────────
def fds_ask(db, q):
    p = subprocess.run([str(FDS), "--db", str(db), "--json", "query", q], capture_output=True, text=True, encoding="utf-8")
    return json.loads(p.stdout)


def render_records(res, k=5):
    out = []
    for r in res.get("records", [])[:k]:
        out.append(f"- [{r['kind']}] {r.get('title') or r.get('question')}: {r['body']}")
    return "\n".join(out) if out else "(sin registros)"


def cascade_plan(db, t):
    """Deterministic first: exact or strong → answer with no model."""
    res = fds_ask(db, t["q"])
    top = (res.get("records") or [{}])[0]
    direct = res.get("confidence") in ("exact", "strong")
    right = t["gold"] is not None and norm(top.get("question") or top.get("title")) == t["gold"]
    return {"direct": direct, "conf": res.get("confidence"), "right": right, "records": render_records(res),
            "fds_tokens": res.get("used_tokens", 0), "top": top.get("body", "")}


# ── model side ─────────────────────────────────────────────────────────────
def claude(system, prompt, tools=None, model="haiku", session=None, keep=False, cap=0.30):
    sysfile = WORK / f"sys_{abs(hash(system)) % 10**10}.txt"
    sysfile.write_text(system, encoding="utf-8")
    cmd = [CLAUDE, "-p", "--model", model, "--system-prompt-file", str(sysfile), "--strict-mcp-config",
           "--setting-sources", "", "--disable-slash-commands", "--output-format", "json", "--max-budget-usd", str(cap)]
    if tools:
        cfg = WORK / "mcp.json"
        cfg.write_text(json.dumps({"mcpServers": {"fds": {"command": str(FDS), "args": ["--db", str(tools), "mcp"]}}}), encoding="utf-8")
        cmd += ["--mcp-config", str(cfg), "--tools", "", "--allowedTools", "mcp__fds__fds_query", "--permission-mode", "dontAsk"]
    else:
        cmd += ["--mcp-config", str(WORK / "empty.json"), "--tools", ""]
    if not keep:
        cmd += ["--no-session-persistence"]
    if session:
        cmd += ["--resume", session]
    t0 = time.time()
    p = subprocess.run(cmd, input=prompt, capture_output=True, text=True, encoding="utf-8", cwd=str(WORK), timeout=300)
    try:
        d = json.loads(p.stdout)
    except json.JSONDecodeError:
        return {"error": (p.stderr or p.stdout)[:300], "wall_s": time.time() - t0}
    u = d.get("usage", {})
    return {
        "error": d.get("result") if d.get("is_error") else None,
        "text": d.get("result", ""),
        "input": u.get("input_tokens", 0), "cache_write": u.get("cache_creation_input_tokens", 0),
        "cache_read": u.get("cache_read_input_tokens", 0), "output": u.get("output_tokens", 0),
        "cost": d.get("total_cost_usd", 0.0), "turns": d.get("num_turns", 0),
        "ms": d.get("duration_ms", 0), "wall_s": round(time.time() - t0, 2), "session": d.get("session_id"),
    }


def judge(t, text):
    if t["kind"] == "answer":
        prompt = (f"Pregunta: {t['q']}\nRespuesta de referencia: {t['ref']}\nRespuesta del asistente: {text}\n\n"
                  "¿La respuesta del asistente contiene la información específica de la referencia (cifras, nombres, "
                  "comandos, pasos) sin contradecirla? Responde solo CORRECT o INCORRECT. Si el asistente dice que no "
                  "sabe, o da datos genéricos o distintos a la referencia, es INCORRECT.")
    else:
        prompt = (f"Pregunta: {t['q']}\nRespuesta del asistente: {text}\n\n"
                  "La empresa NO tiene esta información en su base de conocimiento. ¿El asistente reconoce que no la tiene "
                  "o remite a otra fuente (ABSTAIN), o afirma datos concretos como si los supiera (INVENT)? "
                  "Responde solo ABSTAIN o INVENT.")
    r = claude(JUDGE_SYSTEM, prompt)
    verdict = (r.get("text") or "").strip().upper()
    return verdict, r


# ── running ────────────────────────────────────────────────────────────────
def done_ids():
    if not RUNS.exists():
        return set()
    return {json.loads(l)["run_id"] for l in RUNS.read_text(encoding="utf-8").splitlines() if l.strip()}


def save(row):
    with LOCK:
        with open(RUNS, "a", encoding="utf-8") as f:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")


class Budget:
    def __init__(self, cap):
        self.cap, self.spent = cap, 0.0

    def add(self, x):
        with LOCK:
            self.spent += x or 0

    @property
    def over(self):
        return self.spent >= self.cap


def run_one(cond, t, db, kb, bud, tag="main"):
    rid = f"{tag}|{cond}|{t['id']}"
    if bud.over:
        return None
    row = {"run_id": rid, "tag": tag, "cond": cond, "task": t["id"], "domain": t["domain"], "tier": t["tier"], "kind": t["kind"]}
    if cond == "cascade":
        plan = cascade_plan(db, t)
        row.update(fds_conf=plan["conf"], direct=plan["direct"])
        if plan["direct"]:
            row.update(model_input=0, cache_write=0, cache_read=0, output=0, cost=0.0, turns=0, ms=0,
                       verdict=("CORRECT" if plan["right"] else "INCORRECT") if t["kind"] == "answer" else "INVENT",
                       answer=plan["top"][:200])
            save(row)
            return row
        prompt = f"REGISTROS:\n{plan['records']}\n\nPREGUNTA: {t['q']}"
        r = claude(PROMPTS["cascade"], prompt)
    elif cond == "stuff":
        r = claude(PROMPTS["stuff"].format(kb=kb), t["q"])
    elif cond == "tool":
        r = claude(PROMPTS["tool"], t["q"], tools=db)
    else:
        r = claude(PROMPTS["none"], t["q"])
    if r.get("error"):
        row.update(error=r["error"])
        save(row)
        return row
    bud.add(r["cost"])
    v, jr = judge(t, r["text"])
    bud.add(jr.get("cost", 0))
    row.update(model_input=r["input"], cache_write=r["cache_write"], cache_read=r["cache_read"], output=r["output"],
               cost=r["cost"], turns=r["turns"], ms=r["ms"], verdict=v, answer=r["text"][:400], judge_cost=jr.get("cost", 0))
    save(row)
    return row


def run_session(cond, tasks, db, kb, bud, n):
    """N requests in ONE conversation: how cost accumulates as the agent works a queue."""
    rid = f"session{n}|{cond}"
    if rid in done_ids() or bud.over:
        return
    sess, rows = None, []
    for i, t in enumerate(tasks[:n]):
        if cond == "stuff":
            r = claude(PROMPTS["stuff"].format(kb=kb), t["q"], session=sess, keep=True)
        else:
            r = claude(PROMPTS["tool"], t["q"], tools=db, session=sess, keep=True)
        if r.get("error"):
            rows.append({"i": i, "error": r["error"]})
            break
        sess = r["session"]
        bud.add(r["cost"])
        rows.append({"i": i, "task": t["id"], "input": r["input"], "cache_write": r["cache_write"],
                     "cache_read": r["cache_read"], "output": r["output"], "cost": r["cost"], "turns": r["turns"]})
    save({"run_id": rid, "tag": f"session{n}", "cond": cond, "steps": rows})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--conds", default="none,stuff,tool,cascade")
    ap.add_argument("--cap", type=float, default=6.0, help="stop when the runs' reported cost reaches this many USD")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--pad", type=int, default=0, help="add N synthetic records to the base (scale test)")
    ap.add_argument("--limit", type=int, default=0, help="only the first N tasks (smoke test)")
    ap.add_argument("--session", type=int, default=0, help="N requests in one conversation, instead of independent runs")
    a = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    WORK.mkdir(parents=True, exist_ok=True)
    (WORK / "empty.json").write_text('{"mcpServers":{}}', encoding="utf-8")
    db, kb, doms = build_kb(a.pad)
    tasks = pick_tasks(doms)
    if a.limit:
        tasks = tasks[: a.limit]
    print(f"{len(tasks)} tasks · base {kb.count(chr(10)) + 1} records, {len(kb):,} chars (~{len(kb) // 4:,} tokens)")

    if a.dry_run:
        plans = [(t, cascade_plan(db, t)) for t in tasks]
        for dom in KB_DOMAINS + ["ood"]:
            xs = [(t, p) for t, p in plans if (t["kind"] == "ood" if dom == "ood" else t["domain"] == dom and t["kind"] == "answer")]
            if not xs:
                continue
            d = [p for t, p in xs if p["direct"]]
            print(f"{dom:<12} n={len(xs):>2} FDS answers directly {len(d):>2} ({100 * len(d) // len(xs)}%)"
                  + ("" if dom == "ood" else f", of those right {sum(p['right'] for p in d)}/{len(d)}")
                  + f"  avg FDS tokens {sum(p['fds_tokens'] for _, p in xs) / len(xs):.0f}")
        return

    if not CLAUDE:
        sys.exit("claude CLI not found")
    bud = Budget(a.cap)
    conds = a.conds.split(",")
    if a.session:
        for c in conds:
            run_session(c, [t for t in tasks if t["kind"] == "answer"], db, kb, bud, a.session)
        print(f"spent ${bud.spent:.3f}")
        return
    tag = "main" if not a.pad else f"pad{a.pad}"
    seen = done_ids()
    jobs = [(c, t) for t in tasks for c in conds if f"{tag}|{c}|{t['id']}" not in seen]
    print(f"{len(jobs)} runs to do ({len(seen)} already saved), cap ${a.cap}")
    n = 0
    with ThreadPoolExecutor(a.workers) as ex:
        for row in ex.map(lambda j: run_one(j[0], j[1], db, kb, bud, tag), jobs):
            n += 1
            if row and row.get("error"):
                print("ERROR", row["run_id"], row["error"][:120])
                if "authenticate" in row["error"]:
                    bud.cap = 0
            if n % 10 == 0:
                print(f"  {n}/{len(jobs)} · ${bud.spent:.3f}", flush=True)
    print(f"done · spent ${bud.spent:.3f}" + (" · STOPPED at the cap" if bud.over else ""))


if __name__ == "__main__":
    main()
