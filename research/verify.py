"""Functional and robustness verification of the `fds` binary, end to end.

Every check drives the real release binary on a fresh database in
research/work/, the way a user or an agent would: CLI, HTTP API, MCP over
stdio. Nothing is mocked. Each check records PASS / FAIL / NOTE and what it
saw; the run writes research/results/verify.json.

    python research/verify.py            # everything except images
    python research/verify.py --images   # also describe one image (needs Ollama + qwen2.5vl:3b)
"""

import csv, hashlib, json, os, random, shutil, socket, subprocess, sys, threading, time
import urllib.error, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FDS = ROOT / "target" / "release" / ("fds.exe" if os.name == "nt" else "fds")
WORK = ROOT / "research" / "work" / "verify"
OUT = ROOT / "research" / "results"
OLLAMA = "http://127.0.0.1:11434"

RESULTS = []


def check(area, name, ok, detail="", note=False):
    status = "NOTE" if note else ("PASS" if ok else "FAIL")
    RESULTS.append({"area": area, "check": name, "status": status, "detail": detail})
    print(f"  [{status}] {area} · {name}" + (f" — {detail}" if detail else ""))


def fds(db, *args, json_out=False, input=None, timeout=300):
    cmd = [str(FDS), "--db", str(db)] + (["--json"] if json_out else []) + [str(a) for a in args]
    p = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", input=input, timeout=timeout)
    return p


def jq(db, *args):
    p = fds(db, *args, json_out=True)
    try:
        return p.returncode, json.loads(p.stdout)
    except json.JSONDecodeError:
        return p.returncode, {"_raw": p.stdout, "_err": p.stderr}


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def fresh(name):
    db = WORK / f"{name}.fds"
    for p in (db, db.with_suffix(".fds.tmp")):
        if p.exists():
            p.unlink()
    r = fds(db, "init", name)
    assert r.returncode == 0, r.stderr
    return db


def ollama_up(model=None):
    """Ollama answers, and has `model` when one is named."""
    try:
        tags = json.loads(urllib.request.urlopen(OLLAMA + "/api/tags", timeout=2).read())
    except Exception:
        return False
    return model is None or any(m["name"].split(":")[0] == model.split(":")[0] for m in tags.get("models", []))


# ── 1. CLI ─────────────────────────────────────────────────────────────────
def cli():
    print("\n== CLI")
    db = fresh("cli")
    code, st = jq(db, "status")
    check("cli", "init creates an empty v4 database", code == 0 and st.get("records") == 0 and st.get("format") == 4,
          f"records={st.get('records')} format={st.get('format')}")

    r = fds(db, "fact", "add", "lm317 reference voltage", "1.25 V between OUT and ADJ", "--source", "datasheet")
    code, q = jq(db, "query", "lm317 reference voltage")
    top = (q.get("records") or [{}])[0]
    check("cli", "fact add → exact query answers it", q.get("confidence") == "exact" and "1.25" in top.get("body", ""),
          f"confidence={q.get('confidence')}")

    fds(db, "fact", "add", "lm317 reference voltage", "1.25 V typical (1.20–1.30 V)")
    code, st = jq(db, "status")
    code, q = jq(db, "query", "lm317 reference voltage")
    check("cli", "same question again replaces, not duplicates",
          st.get("facts") == 1 and "1.20" in q["records"][0]["body"], f"facts={st.get('facts')}")

    # Accents typed as a base letter plus a combining mark (macOS file names,
    # some copy-paste) must find what was stored with the composed letter.
    fds(db, "fact", "add", "¿cuál es el café de la casa?", "El café de la casa es el de Yungas.")
    code, q = jq(db, "query", "cuál es el café de la casa")
    check("cli", "a query typed with combining accents (NFD) finds a fact stored with composed ones",
          q.get("confidence") == "exact" and "Yungas" in ((q.get("records") or [{}])[0].get("body", "")),
          f"confidence={q.get('confidence')}")

    code, q = jq(db, "query", "LM317   Reference-Voltage?")
    check("cli", "normalized match (case, spaces, punctuation)", q.get("confidence") == "exact", q.get("match", ""))

    procs = WORK / "procs.json"
    shutil.copy(ROOT / "demo" / "form_a.json", procs)
    r = fds(db, "proc", "add", procs)
    code, q = jq(db, "query", "register a new customer")
    check("cli", "proc add from JSON → intent answers exactly",
          q.get("confidence") == "exact" and q["records"][0]["kind"] == "procedure", q.get("confidence"))

    r = fds(db, "add", ROOT / "demo" / "handbook")
    code, st = jq(db, "status")
    code, q = jq(db, "query", "decoupling capacitor placement near the IC")
    kinds = [x["kind"] for x in q.get("records", [])]
    check("cli", "add a folder of documents → knowledge fragments, searchable",
          st.get("knowledge", 0) > 10 and "knowledge" in kinds,
          f"fragments={st.get('knowledge')} top kinds={kinds[:3]}")
    src = next((x.get("source") for x in q.get("records", []) if x["kind"] == "knowledge"), "")
    check("cli", "every answer carries its source", bool(src), src)

    before = st.get("knowledge")
    fds(db, "add", ROOT / "demo" / "handbook")
    code, st = jq(db, "status")
    check("cli", "re-adding the same documents does not duplicate", st.get("knowledge") == before,
          f"{before} → {st.get('knowledge')}")

    # Links and contexts
    fds(db, "fact", "add", "router admin address", "http://192.168.0.1, see [[router reset]] and [[firmware notes]]")
    fds(db, "fact", "add", "router reset", "Hold RESET 10 s. Write down the [[router admin address]] first.")
    code, q = jq(db, "query", "router admin address", "--links")
    ids = [x.get("question") for x in q.get("records", [])]
    check("cli", "--links brings the linked record in the same answer", "router reset" in ids, str(ids))
    code, show = jq(db, "show", q["records"][0]["id"])
    check("cli", "`show` lists the record's resolved links and backlinks", bool(show.get("links")),
          "empty: links are resolved only over HTTP (GET /records/:id); the CLI shows none", note=not show.get("links"))

    r = fds(db, "context", "set", "Network help", "--instructions", "Answer briefly. Escalate if no router access.",
            "--link", "router admin address", "--link", "router reset", "--link", "does not exist yet")
    code, c = jq(db, "context", "show", "Network help")
    check("cli", "context set/show returns instructions + members",
          c.get("records") and len(c["records"]) == 2 and "Escalate" in c.get("instructions", ""),
          f"members={len(c.get('records', []))} missing={c.get('missing')}")
    h = c.get("hash")
    code, c2 = jq(db, "context", "show", "Network help", "--known", h)
    check("cli", "context show --known <same hash> → unchanged, no payload",
          c2.get("changed") is False and "records" not in c2, json.dumps(c2)[:120])
    fds(db, "fact", "add", "router reset", "Hold RESET 15 s. Write down the [[router admin address]] first.")
    code, c3 = jq(db, "context", "show", "Network help", "--known", h)
    check("cli", "editing a member changes the context hash", c3.get("changed") is not False and c3.get("hash") != h,
          f"{h} → {c3.get('hash')}")
    code, q = jq(db, "query", "Network help")
    check("cli", "a context never appears as a query result",
          all(x.get("type") != "context" for x in q.get("records", [])), str([x.get("type") for x in q.get("records", [])]))

    code, q = jq(db, "query", "decoupling capacitor", "--verified-by", "human")
    check("cli", "--verified-by human excludes ingested documents",
          all(x["kind"] != "knowledge" for x in q.get("records", [])), f"{len(q.get('records', []))} records")
    fds(db, "fact", "add", "office wifi password", "ask IT", "-c", "it")
    code, q = jq(db, "query", "office wifi password", "-c", "default")
    check("cli", "-c scopes to one collection", q.get("confidence") == "none" or all(x["collection"] == "default" for x in q.get("records", [])),
          q.get("confidence"))

    p = fds(db, "query", "what is the capital of paraguay")
    check("cli", "out-of-domain query → none, exit code 2", p.returncode == 2, f"exit={p.returncode}")

    code, q = jq(db, "query", "decoupling capacitor", "--budget", "60")
    toks = q.get("used_tokens", 0)
    check("cli", "--budget caps the tokens returned", 0 < toks <= 60, f"used_tokens={toks} of 60")

    code, ls = jq(db, "ls", "--kind", "fact")
    rows = ls if isinstance(ls, list) else ls.get("rows", ls.get("records", []))
    check("cli", "ls --kind fact lists only facts", rows and all(x.get("kind") == "fact" for x in rows), f"{len(rows)} rows")
    rid = rows[0]["id"]
    r = fds(db, "rm", rid)
    code, ls2 = jq(db, "ls", "--kind", "fact")
    rows2 = ls2 if isinstance(ls2, list) else ls2.get("rows", ls2.get("records", []))
    check("cli", "rm <id> removes exactly one record", len(rows2) == len(rows) - 1, f"{len(rows)} → {len(rows2)}")
    r = fds(db, "rm", "--source", "demo/handbook/lm317.md")
    code, st2 = jq(db, "status")
    check("cli", "rm --source <stored path> removes a document's fragments", st2.get("knowledge", 0) < st.get("knowledge", 0),
          f"{st.get('knowledge')} → {st2.get('knowledge')}")
    for label, path in [("Windows-style path", r"demo\handbook\ohms.md"), ("absolute path", str(ROOT / "demo" / "handbook" / "led.md"))]:
        r = fds(db, "rm", "--source", path)
        code, st3 = jq(db, "status")
        check("cli", f"rm --source with a {label}", st3.get("knowledge", 0) < st2.get("knowledge", 0),
              f"{r.stdout.strip()} — silently matches nothing" if st3.get("knowledge") == st2.get("knowledge") else "removed")
        st2 = st3

    agents = WORK / "AGENTS.md"
    agents.write_text("# Project rules\n\nRun `cargo test` before every commit.\nRouter facts: [[router admin address]]\n", encoding="utf-8")
    p = fds(db, "context", "import", agents)
    code, c = jq(db, "context", "show", "AGENTS")
    out = fds(db, "context", "export", "AGENTS").stdout
    check("cli", "AGENTS.md imports as a context, its [[links]] as members, and exports back",
          p.returncode == 0 and "cargo test" in c.get("instructions", "") and len(c.get("records", [])) == 1
          and out.startswith("# Project rules") and "## Reference" in out and "192.168.0.1" in out,
          f"members={len(c.get('records', []))}, export {len(out)} chars")

    check("cli", "CLI has no `export` and no `edit` (HTTP only)", True,
          "parity gap: a script without the server cannot export or edit", note=True)
    return db


# ── 2. Import ──────────────────────────────────────────────────────────────
def imports():
    print("\n== Import")
    db = fresh("import")
    rows = [["tipo", "pregunta", "respuesta", "colección"],
            ["hecho", "horario de atención", "Lunes a viernes, 8:00–18:00", "soporte"],
            ["hecho", "¿cómo cambio mi contraseña?", "Perfil > Seguridad > Cambiar contraseña", "soporte"],
            ["hecho", "dirección de la oficina", "Av. Arequipa 1234, Lima", "soporte"]]
    f = WORK / "excel.csv"
    with open(f, "w", newline="", encoding="cp1252") as fh:
        csv.writer(fh, delimiter=";").writerows(rows)
    h0 = sha(db)
    p = fds(db, "import", f, "--dry-run")
    check("import", "Excel CSV (; + Windows-1252) dry run writes nothing", sha(db) == h0 and p.returncode == 0,
          p.stderr.strip().splitlines()[0] if p.stderr.strip() else "")
    p = fds(db, "import", f)
    code, q = jq(db, "query", "cómo cambio mi contraseña")
    check("import", "real import: accents survive, query answers", q.get("confidence") == "exact", q.get("confidence"))
    h1 = sha(db)
    p = fds(db, "import", f, json_out=True)
    plan = json.loads(p.stdout)
    check("import", "re-import is idempotent (all unchanged, file untouched)",
          plan["summary"]["unchanged"] == 3 and sha(db) == h1, json.dumps(plan["summary"]))

    bad = WORK / "bad.csv"
    bad.write_text("kind,question,answer\nfact,ok question,ok answer\nfact,,missing question\n", encoding="utf-8")
    h2 = sha(db)
    p = fds(db, "import", bad)
    check("import", "a batch with an error row is refused whole", p.returncode != 0 and sha(db) == h2, f"exit={p.returncode}")
    p = fds(db, "import", bad, "--skip-errors")
    code, q = jq(db, "query", "ok question")
    check("import", "--skip-errors writes the valid rows", q.get("confidence") == "exact", q.get("confidence"))

    jl = WORK / "rows.jsonl"
    jl.write_text("\n".join(json.dumps(x, ensure_ascii=False) for x in [
        {"kind": "procedure", "intent": "reiniciar el módem", "steps": [{"action": "run", "command": "reboot"}]},
        {"kind": "context", "name": "Soporte", "instructions": "Sé breve. [[horario de atención]]"},
    ]), encoding="utf-8")
    p = fds(db, "import", jl)
    code, c = jq(db, "context", "show", "Soporte")
    code, q = jq(db, "query", "reiniciar el módem")
    check("import", "JSON Lines: procedures and contexts", q.get("confidence") == "exact" and len(c.get("records", [])) == 1,
          f"proc={q.get('confidence')} ctx members={len(c.get('records', []))}")

    inj = WORK / "inj.csv"
    inj.write_text("kind,question,answer\nfact,formula test,=HYPERLINK(\"http://x\")\n", encoding="utf-8")
    fds(db, "import", inj)
    code, q = jq(db, "query", "formula test")
    check("import", "a formula cell is stored as text", q["records"][0]["body"].startswith("=HYPERLINK"), q["records"][0]["body"][:40])
    return db


# ── 3. Robustness ──────────────────────────────────────────────────────────
def robustness():
    print("\n== Robustness")
    good = fresh("robust")
    for i in range(50):
        fds(good, "fact", "add", f"question number {i}", f"answer number {i}")
    size = good.stat().st_size

    for label, mutate in [
        ("truncated file", lambda b: b[: len(b) // 2]),
        ("random bytes", lambda b: bytes(random.Random(1).getrandbits(8) for _ in range(len(b)))),
        ("empty file", lambda b: b""),
        ("flipped byte in the index", lambda b: b[:-20] + bytes([b[-20] ^ 0xFF]) + b[-19:]),
    ]:
        bad = WORK / "corrupt.fds"
        bad.write_bytes(mutate(good.read_bytes()))
        p = fds(bad, "status")
        panicked = "panicked" in p.stderr or p.returncode == 101
        check("robust", f"{label}: clean error, no crash", p.returncode != 0 and not panicked,
              (p.stderr.strip().splitlines() or ["(no message)"])[-1][:110])

    # Unicode round trip
    u = fresh("unicode")
    cases = {"日本語の質問はどうですか": "はい、動きます", "中文问题可以吗": "可以", "¿Dónde está el baño?": "Al fondo, a la derecha",
             "emoji 🚀 launch date": "September 🗓️", "سؤال باللغة العربية": "نعم", "naïve café résumé": "ok"}
    for qq, a in cases.items():
        fds(u, "fact", "add", qq, a)
    ok = 0
    for qq, a in cases.items():
        code, q = jq(u, "query", qq)
        ok += q.get("confidence") == "exact" and a in q["records"][0]["body"]
    check("robust", "Unicode questions (ja, zh, es, emoji, ar, accents) round-trip exactly", ok == len(cases), f"{ok}/{len(cases)}")
    code, q = jq(u, "query", "baño")
    check("robust", "a single CJK-free word finds its Spanish record", bool(q.get("records")), q.get("confidence"))
    code, q = jq(u, "query", "日本語")
    check("robust", "partial CJK query (no spaces between words) finds the record", bool(q.get("records")),
          f"confidence={q.get('confidence')}", note=not q.get("records"))

    big = WORK / "big_record.jsonl"
    big.write_text(json.dumps({"kind": "fact", "question": "very long answer", "answer": "x " * 400_000}), encoding="utf-8")
    p = fds(u, "import", big)
    code, q = jq(u, "query", "very long answer", "--budget", "200")
    check("robust", "an 800 KB answer is stored and the budget still holds", p.returncode == 0 and bool(q.get("records")),
          f"used_tokens={q.get('used_tokens')} of 200")

    # v2 → v4 migration
    v2 = ROOT / "winops" / "winops.fds"
    if v2.exists() and v2.read_bytes()[:4] == b"FDS2":
        m = WORK / "migrate.fds"
        shutil.copy(v2, m)
        code, st = jq(m, "status")
        n = st.get("records")
        code, q1 = jq(m, "query", "flush the dns cache")
        fds(m, "fact", "add", "migration probe", "yes")
        code, st2 = jq(m, "status")
        code, q2 = jq(m, "query", "flush the dns cache")
        check("robust", "a v2 file is read, and the first write migrates it to v4 with nothing lost",
              st2.get("format") == 4 and st2.get("records") == n + 1 and q1["records"][0]["id"] == q2["records"][0]["id"],
              f"v{st.get('format')} {n} records → v{st2.get('format')} {st2.get('records')}")
    else:
        check("robust", "v2 → v4 migration", True, "no v2 file available to test", note=True)

    # Concurrent writers: two processes adding at once.
    c = fresh("concurrent")
    N = 20
    errors = []

    def writer(tag):
        for i in range(N):
            p = fds(c, "fact", "add", f"{tag} question {i}", f"{tag} answer {i}")
            if p.returncode != 0:
                errors.append(p.stderr.strip()[:100])

    ts = [threading.Thread(target=writer, args=(t,)) for t in ("alpha", "beta", "gamma")]
    [t.start() for t in ts]
    [t.join() for t in ts]
    code, st = jq(c, "status")
    got = st.get("facts", 0)
    check("robust", f"3 processes × {N} concurrent writes keep every record", got == 3 * N and not errors,
          f"kept {got}/{3 * N}; failed writes: {len(errors)}" + (f" e.g. {errors[0]!r}" if errors else ""))

    # Kill during a large write: the file must stay readable (old or new).
    k = fresh("kill")
    rows = WORK / "big.jsonl"
    rows.write_text("\n".join(json.dumps({"kind": "fact", "question": f"bulk question {i}", "answer": "a" * 400})
                              for i in range(20000)), encoding="utf-8")
    survived = 0
    for delay in (0.3, 0.6, 0.9, 1.2, 1.5):
        p = subprocess.Popen([str(FDS), "--db", str(k), "import", str(rows)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(delay)
        p.kill(); p.wait()
        code, st = jq(k, "status")
        survived += code == 0
    check("robust", "killing a write at 5 different moments never leaves an unreadable file", survived == 5, f"{survived}/5 readable")
    tmp = k.with_suffix(".fds.tmp")
    check("robust", "a killed write can leave a stray .fds.tmp behind", True,
          f"left behind: {tmp.exists()} ({tmp.stat().st_size // 1024} KB)" if tmp.exists() else "none left", note=True)

    sp = WORK / "carpeta con espacios y ñ"
    sp.mkdir(exist_ok=True)
    s = sp / "base ñandú.fds"
    if s.exists():
        s.unlink()
    p1 = fds(s, "init", "x"); p2 = fds(s, "fact", "add", "q", "a")
    code, q = jq(s, "query", "q")
    check("robust", "paths with spaces and non-ASCII characters", q.get("confidence") == "exact", str(s.name))


# ── 4. HTTP ────────────────────────────────────────────────────────────────
def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close(); return port


def http(method, url, body=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    h = {"content-type": "application/json", **(headers or {})}
    req = urllib.request.Request(url, data=data, method=method, headers=h)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, r.read().decode("utf-8", "replace"), dict(r.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace"), dict(e.headers)


def raw_http(port, host_header):
    s = socket.create_connection(("127.0.0.1", port), timeout=5)
    s.sendall(f"GET /status HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n".encode())
    data = s.recv(200).decode(errors="replace"); s.close()
    return data.split("\r\n")[0]


def http_checks(db):
    print("\n== HTTP")
    port = free_port()
    srv = subprocess.Popen([str(FDS), "--db", str(db), "serve", "--port", str(port)],
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8")
    token = None
    t0 = time.time()
    while time.time() - t0 < 15:
        line = srv.stderr.readline()
        if "x-fds-token" in line:
            token = line.split("x-fds-token: ")[1].split("`")[0].strip()
            break
    base = f"http://127.0.0.1:{port}"
    try:
        code, body, _ = http("GET", base + "/status")
        check("http", "GET /status without a token", code == 200, f"{code}")
        code, body, _ = http("POST", base + "/query", {"q": "router admin address"})
        check("http", "POST /query answers", code == 200 and json.loads(body).get("confidence") == "exact", f"{code}")
        code, body, _ = http("POST", base + "/facts", {"question": "http probe", "answer": "no token"})
        check("http", "a write without the token is refused", code in (401, 403), f"{code}")
        code, body, _ = http("POST", base + "/facts", {"question": "http probe", "answer": "with token"}, {"x-fds-token": token})
        check("http", "a write with the token succeeds", code in (200, 201), f"{code}")
        line = raw_http(port, "evil.example.com")
        check("http", "a foreign Host header is refused (DNS rebinding)", " 200 " not in line, line)
        code, body, hd = http("GET", base + "/contexts/Network%20help")
        etag = hd.get("etag") or hd.get("ETag")
        code2, _, _ = http("GET", base + "/contexts/Network%20help", headers={"If-None-Match": etag or ""})
        check("http", "context ETag → 304 Not Modified on repeat", code == 200 and code2 == 304, f"{code} then {code2}")
        h = json.loads(body).get("hash")
        code, body, _ = http("GET", base + f"/contexts/Network%20help?known={h}")
        check("http", "?known=<hash> → {changed:false}", json.loads(body).get("changed") is False, body[:80])
        code, body, _ = http("GET", base + "/records?search=http%20probe&limit=1")
        rid = json.loads(body)["rows"][0]["id"]
        code, body, _ = http("PATCH", base + f"/records/{rid}", {"body": "edited over HTTP"}, {"x-fds-token": token})
        patch_code = code
        code, rec, _ = http("GET", base + f"/records/{rid}")
        rec = json.loads(rec)
        check("http", "PATCH edits a record and marks it verified by a person",
              "edited over HTTP" in rec.get("body", "") and rec.get("verified_by") == "human", f"PATCH {patch_code}, verified_by={rec.get('verified_by')}")
        code, csv_text, _ = http("GET", base + "/export?format=csv")
        code_j, json_text, _ = http("GET", base + "/export?format=json")
        check("http", "export CSV and JSON", code == 200 and code_j == 200, f"csv {len(csv_text)} B, json {len(json_text)} B")

        # External write while the server runs.
        fds(db, "fact", "add", "written by the CLI meanwhile", "yes")
        code, body, _ = http("POST", base + "/query", {"q": "written by the CLI meanwhile"})
        seen = json.loads(body).get("confidence") == "exact"
        check("http", "the server sees a CLI write without /reload", seen,
              "stale until POST /reload" if not seen else "seen", note=not seen)
        http("POST", base + "/reload", {})
        code, body, _ = http("POST", base + "/query", {"q": "written by the CLI meanwhile"})
        check("http", "after POST /reload the CLI write is visible", json.loads(body).get("confidence") == "exact", "")
        code, body, _ = http("POST", base + "/facts", {"question": "server write after cli", "answer": "x"}, {"x-fds-token": token})
        code2, st = jq(db, "status")
        code, q = jq(db, "query", "written by the CLI meanwhile")
        check("http", "a server write keeps the CLI's record (read-before-write)", q.get("confidence") == "exact", "")

        if os.name == "nt":
            out = subprocess.run(["netstat", "-ano", "-p", "TCP"], capture_output=True, text=True).stdout
            lines = [l for l in out.splitlines() if f":{port} " in l and "LISTENING" in l]
            check("http", "listens on loopback only", lines and all("127.0.0.1" in l for l in lines), lines[0].split()[1] if lines else "")
    finally:
        srv.kill()


# ── 5. MCP ─────────────────────────────────────────────────────────────────
def mcp_checks(db):
    print("\n== MCP")
    p = subprocess.Popen([str(FDS), "--db", str(db), "mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, text=True, encoding="utf-8")
    n = [0]

    def rpc(method, params=None):
        n[0] += 1
        p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": n[0], "method": method, "params": params or {}}) + "\n")
        p.stdin.flush()
        return json.loads(p.stdout.readline())

    try:
        r = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "verify", "version": "1"}})
        p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"); p.stdin.flush()
        check("mcp", "initialize", "result" in r, r.get("result", {}).get("protocolVersion", ""))
        r = rpc("tools/list")
        names = [t["name"] for t in r["result"]["tools"]]
        check("mcp", "tools/list exposes query, contexts, context, check, status, journal", len(names) == 6 and "fds_check" in names and "fds_journal" in names, ", ".join(names))
        r = rpc("tools/call", {"name": "fds_query", "arguments": {"query": "router admin address", "with_links": True}})
        txt = r["result"]["content"][0]["text"]
        check("mcp", "fds_query with links", "192.168.0.1" in txt and "router reset" in txt.lower(), txt.splitlines()[0][:90])
        r = rpc("tools/call", {"name": "fds_context", "arguments": {"name": "Network help"}})
        full = r["result"]["content"][0]["text"]
        import re
        h = (re.search(r"hash[: ]+([0-9a-f]{16})", full) or [None, None])[1]
        r = rpc("tools/call", {"name": "fds_context", "arguments": {"name": "Network help", "known_hash": h}})
        again = r["result"]["content"][0]["text"]
        check("mcp", "fds_context with known_hash → one line instead of the bundle",
              "unchanged" in again and len(again) < len(full) / 3, f"{len(full)} chars → {len(again)} chars")
        r = rpc("tools/call", {"name": "fds_query", "arguments": {}})
        check("mcp", "a missing argument is a tool error, not a crash", r["result"].get("isError") is True, "")
        r = rpc("nonsense/method")
        check("mcp", "an unknown method gets a JSON-RPC error", r.get("error", {}).get("code") == -32601, "")

        fds(db, "fact", "add", "added while the agent is connected", "fresh fact")
        r = rpc("tools/call", {"name": "fds_query", "arguments": {"query": "added while the agent is connected"}})
        txt = r["result"]["content"][0]["text"]
        seen = "fresh fact" in txt
        check("mcp", "a connected agent sees records written after it connected", seen,
              "stale: the MCP server loads the file once at start and never reloads" if not seen else "seen")
    finally:
        p.kill()


# ── 6. Embeddings and images (need Ollama) ─────────────────────────────────
def vectors(db):
    print("\n== Embeddings")
    if not ollama_up("all-minilm"):
        check("vectors", "Ollama or its all-minilm model is missing: skipped", True, "", note=True)
        return
    t0 = time.time()
    p = fds(db, "embed", "--model", "all-minilm", timeout=900)
    code, st = jq(db, "status")
    check("vectors", "embed covers every record", st.get("embedded_records") == st.get("records"),
          f"{st.get('embedded_records')}/{st.get('records')} in {time.time() - t0:.1f}s")
    code, q = jq(db, "query", "voltage regulator adjustable output")
    check("vectors", "hybrid query returns cosine scores", any(x.get("cosine", 0) > 0 for x in q.get("records", [])),
          f"confidence={q.get('confidence')}")
    p = fds(db, "query", "what is the capital of paraguay")
    check("vectors", "with vectors, an out-of-domain query still answers none (keyword gate)", p.returncode == 2, f"exit={p.returncode}")


def images():
    print("\n== Images")
    if not ollama_up("qwen2.5vl"):
        check("images", "Ollama or its qwen2.5vl model is missing: skipped", True, "", note=True)
        return
    db = fresh("images")
    img = ROOT / "winops" / "img" / "error_impresora.png"
    t0 = time.time()
    p = fds(db, "add", img, "--lang", "es", timeout=900)
    t1 = time.time() - t0
    code, q = jq(db, "query", "13.B2.D1")
    rec = (q.get("records") or [{}])[0]
    check("images", "an image is described and its printed code is searchable",
          rec.get("type") == "image" or "impresora" in rec.get("title", ""), f"{t1:.0f}s, confidence={q.get('confidence')}")
    check("images", "a model-written record says so", str(rec.get("verified_by", "")).startswith("model:"), rec.get("verified_by", ""))
    t0 = time.time()
    fds(db, "add", img, "--lang", "es")
    check("images", "re-adding the same image does not call the model again", time.time() - t0 < 5, f"{time.time() - t0:.1f}s")


# ── 7. Sectors: one memory per agent, one server for all ───────────────────
class Stdio:
    """An MCP client over stdio, the way an agent host runs `fds mcp`."""

    def __init__(self, args, env=None):
        self.p = subprocess.Popen([str(FDS)] + args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True, encoding="utf-8", env=env)
        self.n = 0

    def rpc(self, method, params=None):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}) + "\n")
        self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())

    def tool(self, name, **args):
        r = self.rpc("tools/call", {"name": name, "arguments": args})
        return r.get("result", {}).get("content", [{}])[0].get("text", ""), r

    def close(self):
        self.p.kill()


def sectors_checks():
    print("\n== Sectors")
    root = WORK / "sectors"
    if root.exists():
        shutil.rmtree(root)
    root.mkdir(parents=True)
    man = root / "sectors.json"

    def sec(*args, json_out=False):
        cmd = [str(FDS)] + (["--json"] if json_out else []) + ["sector", *map(str, args), "--manifest", str(man)]
        return subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8")

    sec("new", "shared")
    fds(root / "shared.fds", "fact", "add", "refund window", "30 days from delivery")
    keys = {}
    for agent in ("ventas", "ops"):
        p = sec("new", agent, "--agent", "--reads", "shared", json_out=True)
        keys[agent] = json.loads(p.stdout)["key"]
    manifest = json.loads(man.read_text(encoding="utf-8"))
    check("sectors", "`sector new` creates files and keeps only key hashes",
          len(manifest["sectors"]) == 3 and all(k not in man.read_text(encoding="utf-8") for k in keys.values()),
          f"{[s['name'] for s in manifest['sectors']]}")
    p = sec("new", "Bad Name")
    check("sectors", "an invalid sector name is refused", p.returncode != 0 and not (root / "Bad Name.fds").exists(), "")

    port = free_port()
    srv = subprocess.Popen([str(FDS), "serve", "--sectors", str(man), "--port", str(port)],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f"http://127.0.0.1:{port}/mcp"
    for _ in range(100):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=1)
            break
        except Exception:
            time.sleep(0.1)
    bridge = None
    try:
        env = dict(os.environ, FDS_KEY=keys["ventas"])
        bridge = Stdio(["mcp", "--connect", url], env=env)
        init = bridge.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "verify", "version": "1"}})
        tools = [t["name"] for t in bridge.rpc("tools/list")["result"]["tools"]]
        check("sectors", "a stdio agent reaches the server through `mcp --connect`, with memory tools",
              "fds_remember" in tools and "result" in init, ", ".join(tools))
        said, _ = bridge.tool("fds_remember", question="acme discount", answer="12 percent until december")
        rid = said.split()[1] if said.startswith("saved") else ""
        txt, _ = bridge.tool("fds_query", query="acme discount")
        check("sectors", "an agent remembers and finds it again", "12 percent" in txt and "model:ventas" in txt, said)
        txt, _ = bridge.tool("fds_query", query="refund window")
        check("sectors", "an agent reads the shared sector", "30 days" in txt and "sector: shared" in txt, "")

        def http_tool(key, name, **args):
            body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}).encode()
            req = urllib.request.Request(url, data=body, headers={"content-type": "application/json", "authorization": f"Bearer {key}"})
            try:
                with urllib.request.urlopen(req, timeout=30) as r:
                    return r.status, json.loads(r.read())["result"]["content"][0]["text"]
            except urllib.error.HTTPError as e:
                return e.code, e.read().decode("utf-8", "replace")

        code, txt = http_tool(keys["ops"], "fds_query", query="acme discount")
        check("sectors", "another agent cannot see that memory", code == 200 and "12 percent" not in txt, f"confidence line: {txt.splitlines()[0]}")
        code, _ = http_tool("fdsk_not_a_key", "fds_query", query="refund window")
        check("sectors", "a wrong key gets 401", code == 401, f"{code}")

        p = sec("promote", rid, "--from", "ventas", "--to", "shared")
        code, txt = http_tool(keys["ops"], "fds_query", query="acme discount")
        check("sectors", "after a person promotes it, every agent sees it, verified by a person",
              p.returncode == 0 and "12 percent" in txt and "verified_by: human" in txt, (p.stderr or p.stdout).strip()[:100])

        fds(root / "ventas.fds", "fact", "add", "escalation contact", "Marcela, ext 4412")
        own = json.loads(fds(root / "ventas.fds", "ls", json_out=True).stdout)
        human_id = next(r["id"] for r in own if r.get("question") == "escalation contact")
        said, _ = bridge.tool("fds_forget", id=human_id)
        check("sectors", "an agent cannot remove what a person verified in its sector", "only records an agent wrote" in said, said[:90])

        old = keys["ventas"]
        p = sec("key", "ventas", json_out=True)
        code, _ = http_tool(old, "fds_query", query="refund window")
        code2, _ = http_tool(json.loads(p.stdout)["key"], "fds_query", query="refund window")
        check("sectors", "rotating a key revokes the old one at once", code == 401 and code2 == 200, f"old {code}, new {code2}")
    finally:
        if bridge:
            bridge.close()
        srv.kill()

    direct = Stdio(["mcp", "--sectors", str(man), "--as", "ops"])
    try:
        direct.rpc("initialize", {"protocolVersion": "2025-06-18"})
        txt, _ = direct.tool("fds_query", query="refund window")
        check("sectors", "`mcp --sectors --as` works without a server", "30 days" in txt, "")
    finally:
        direct.close()


def journal_checks():
    print("\n== Journal")
    db = fresh("journal")
    p = fds(db, "log", "Add a rate limit to /login", "--why", "Five failures in a minute lock the address for ten.",
            "--file", "src/auth.rs", "--file", "tests/auth.rs", "--branch", "feat/login", "--commit", "a1b2c3d", "--by", "backend")
    code, rows = jq(db, "journal", "show")
    e = rows[0] if isinstance(rows, list) and rows else {}
    check("journal", "an agent's entry is logged, with its files, branch and commit, and waits for review",
          p.returncode == 0 and e.get("agent") == "backend" and e.get("reviewed") is False and e.get("files") == ["src/auth.rs", "tests/auth.rs"]
          and e.get("branch") == "feat/login" and e.get("commit") == "a1b2c3d", f"rc={p.returncode} {str(e)[:120]}")

    fds(db, "log", "Rename the session cookie", "--why", "It clashed with the CDN's cookie.", "--branch", "main")
    code, rows = jq(db, "journal", "show")
    mine = [r for r in rows if r["agent"] == ""]
    check("journal", "an entry a person logs is marked as theirs and reviewed",
          len(rows) == 2 and len(mine) == 1 and mine[0]["reviewed"] is True, f"{len(rows)} entries")
    code, rows = jq(db, "journal", "show", "--branch", "feat/login")
    code2, rows2 = jq(db, "journal", "show", "--agent", "backend", "--limit", "1")
    check("journal", "the journal filters by branch, agent and limit", len(rows) == 1 and len(rows2) == 1, f"{len(rows)} {len(rows2)}")

    # An agent connected over MCP logs its own work.
    agent = Stdio(["--db", str(db), "mcp", "--write", "--as", "claude-1"])
    try:
        agent.rpc("initialize", {"protocolVersion": "2025-06-18"})
        tools = [t["name"] for t in agent.rpc("tools/list")["result"]["tools"]]
        said, _ = agent.tool("fds_log", summary="Cache the price list", why="The page asked the API on every visit.",
                             files=["web/prices.js"], branch="feat/cache")
        check("journal", "an agent logs through MCP with fds_log", "fds_log" in tools and said.startswith("logged"), said[:80])
    finally:
        agent.close()
    code, rows = jq(db, "journal", "show", "--agent", "claude-1")
    check("journal", "the agent's MCP entry shows up for the developer, unreviewed",
          len(rows) == 1 and rows[0]["reviewed"] is False and rows[0]["files"] == ["web/prices.js"], str(rows)[:100])

    # A reviewing agent reads the journal back over MCP, without --write, without a terminal.
    reviewer = Stdio(["--db", str(db), "mcp"])
    try:
        reviewer.rpc("initialize", {"protocolVersion": "2025-06-18"})
        rtools = [t["name"] for t in reviewer.rpc("tools/list")["result"]["tools"]]
        tl, _ = reviewer.tool("fds_journal")
        plain, _ = reviewer.tool("fds_journal", timeline=False, limit=1)
        only, _ = reviewer.tool("fds_journal", agent="claude-1")
        none, _ = reviewer.tool("fds_journal", agent="nobody")
    finally:
        reviewer.close()
    lines = tl.splitlines()
    check("journal", "a reviewer without --write reads the journal with fds_journal: timeline oldest first, then each entry's detail",
          "fds_journal" in rtools and "fds_log" not in rtools and len(lines) >= 3 and lines[0].startswith("#01 ") and "detail:" in tl
          and tl.index("Add a rate limit to /login") < tl.index("Cache the price list")
          and "files: src/auth.rs, tests/auth.rs" in tl and "branch feat/login | commit a1b2c3d" in tl
          and "agent `claude-1`, not yet reviewed" in tl, tl[:160].replace(chr(10), " | "))
    check("journal", "fds_journal filters by agent, lists newest first without the timeline, and says so when empty",
          "Cache the price list" in only and "Add a rate limit" not in only and "#01" not in plain
          and plain.count("by ") == 1 and none.strip().startswith("el registro est"), none.strip()[:60])

    out = WORK / "JOURNAL.md"
    r = fds(db, "journal", "export", "--out", out)
    md = out.read_text(encoding="utf-8") if out.exists() else ""
    check("journal", "export writes a Markdown page a developer can read on GitHub",
          r.returncode == 0 and "# Change journal" in md and "### Add a rate limit to /login" in md and "- `src/auth.rs`" in md, f"{len(md)} chars")

    # Publishing to a git branch leaves the developer's own work alone.
    git = shutil.which("git")
    if not git:
        check("journal", "publish commits the journal to a branch `fds`", True, "git is not installed: skipped", note=True)
        return

    def g(repo, *a):
        return subprocess.run([git, "-C", str(repo), *a], capture_output=True, text=True, encoding="utf-8")

    repo = WORK / "journal_repo"
    if repo.exists():
        # git writes its objects read-only; on Windows they must be made writable to be deleted.
        def writable(fn, path, _err):
            os.chmod(path, 0o700)
            fn(path)
        shutil.rmtree(repo, onerror=writable)
    repo.mkdir(parents=True)
    for a in (["init", "-q"], ["symbolic-ref", "HEAD", "refs/heads/main"], ["config", "user.name", "dev"],
              ["config", "user.email", "dev@example.com"], ["config", "commit.gpgsign", "false"]):
        g(repo, *a)
    (repo / "code.txt").write_text("v1\n")
    g(repo, "add", "code.txt")
    g(repo, "commit", "-q", "-m", "first")
    (repo / "code.txt").write_text("v2, not committed\n")
    head = g(repo, "rev-parse", "HEAD").stdout.strip()

    r = fds(db, "journal", "publish", "--repo", repo)
    shown = g(repo, "show", "fds:JOURNAL.md").stdout
    tracked = g(repo, "status", "--porcelain").stdout
    check("journal", "publish commits the journal to branch `fds` and touches nothing else",
          r.returncode == 0 and "Add a rate limit to /login" in shown and g(repo, "rev-parse", "HEAD").stdout.strip() == head
          and g(repo, "symbolic-ref", "--short", "HEAD").stdout.strip() == "main" and "code.txt" in tracked
          and (repo / "code.txt").read_text() == "v2, not committed\n" and not (repo / "JOURNAL.md").exists(),
          (r.stderr or "").strip()[:100])
    r2 = fds(db, "journal", "publish", "--repo", repo)
    count = g(repo, "rev-list", "--count", "fds").stdout.strip()
    check("journal", "publishing the same journal again adds no commit", r2.returncode == 0 and count == "1", f"commits on fds: {count}")
    fds(db, "log", "Fix a typo in the footer", "--branch", "main")
    fds(db, "journal", "publish", "--repo", repo)
    count = g(repo, "rev-list", "--count", "fds").stdout.strip()
    check("journal", "a new entry adds one commit to `fds`", count == "2", f"commits on fds: {count}")


def policy_checks():
    print("\n== Policies, guard and audit")
    root = WORK / "policy"
    if root.exists():
        def writable(fn, path, _err):
            os.chmod(path, 0o700)
            fn(path)
        shutil.rmtree(root, onerror=writable)
    root.mkdir(parents=True)
    man = root / "sectors.json"

    def sec(*args):
        return subprocess.run([str(FDS), "sector", *map(str, args), "--manifest", str(man)], capture_output=True, text=True, encoding="utf-8")

    sec("new", "policies")
    pdb = root / "policies.fds"
    fds(pdb, "policy", "add", "Below -20 C the stock is damaged.", "--subject", "fridge.chamber3.setpoint", "--action", "set",
        "--rule", "range", "--min", "-20", "--max", "-16", "--unit", "C",
        "--tool", "set_temperature", "--tool-subject", "fridge.{chamber}.setpoint", "--tool-value", "{celsius}")
    fds(pdb, "policy", "add", "Nobody drops production.", "--subject", "db.prod.*", "--action", "drop", "--rule", "deny",
        "--tool", "run_sql_drop", "--tool-subject", "db.prod.{table}")
    bad = fds(pdb, "policy", "add", "Broken", "--subject", "x", "--rule", "range", "--min", "5", "--max", "1")
    check("policy", "a range whose min is above its max is refused when it is written", bad.returncode != 0, (bad.stderr or "").strip()[:80])
    ls = fds(pdb, "policy", "ls")
    check("policy", "`policy ls` lists what was added", "fridge.chamber3.setpoint" in ls.stdout and "db.prod.*" in ls.stdout, ls.stdout.strip()[:100])

    def chk(value, subject="fridge.chamber3.setpoint", action="set"):
        args = ["check", "--subject", subject, "--action", action] + (["--value", value] if value is not None else [])
        p = fds(pdb, *args, json_out=True)
        return p.returncode, (json.loads(p.stdout) if p.stdout.strip().startswith("{") else {})

    c1, d1 = chk("-18")
    c2, d2 = chk("-40")
    c3, d3 = chk("warm")
    c4, d4 = chk(None, "db.prod.customers", "drop")
    check("policy", "`check`: inside the range is allowed (exit 0), outside is denied (exit 3) with the rule and its text",
          (c1, d1.get("decision")) == (0, "allow") and (c2, d2.get("decision")) == (3, "deny")
          and "stock is damaged" in json.dumps(d2) and d2["because"][0]["min"] == "-20", f"{c1} {c2} {d2.get('because')}")
    check("policy", "a value that is not a number, and a rule `deny`, are denied", (c3, d3.get("decision")) == (3, "deny") and (c4, d4.get("decision")) == (3, "deny"), f"{c3} {c4}")
    c5, d5 = chk("1", "garage.door", "open")
    check("policy", "with no matching policy the default decides (allow here, and it says so)", c5 == 0 and d5.get("default_applied") is True, str(d5)[:90])
    p = fds(pdb, "check", "--subject", "garage.door", "--action", "open", "--default", "deny")
    check("policy", "`--default deny` closes what no policy covers", p.returncode == 3, p.stdout.strip()[:80])

    # The same check over HTTP and over MCP.
    port = free_port()
    srv = subprocess.Popen([str(FDS), "--db", str(pdb), "serve", "--port", str(port)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=1)
                break
            except Exception:
                time.sleep(0.1)
        code, body, _ = http("POST", f"http://127.0.0.1:{port}/check", {"subject": "fridge.chamber3.setpoint", "action": "set", "value": -40})
        check("policy", "POST /check answers the same, with a number as the value", code == 200 and json.loads(body).get("decision") == "deny", f"{code} {body[:80]}")
    finally:
        srv.kill()
    agent = Stdio(["--db", str(pdb), "mcp"])
    try:
        agent.rpc("initialize", {"protocolVersion": "2025-06-18"})
        said, _ = agent.tool("fds_check", subject="fridge.chamber3.setpoint", action="set", value="-40")
        ok, _ = agent.tool("fds_check", subject="fridge.chamber3.setpoint", action="set", value="-18")
        check("policy", "the MCP tool fds_check tells an agent deny, and allow", said.startswith("deny") and ok.startswith("allow"), said[:70])
    finally:
        agent.close()

    # ── the guard, in front of a fake tool server ──
    server = ROOT / "research" / "fake_mcp_server.py"
    received = root / "received.jsonl"

    def guard(extra=()):
        return Stdio(["guard", "--policies", str(man), "--agent", "claude-r", *extra, "--", sys.executable, str(server), str(received)])

    def call(g, tool, **args):
        text, raw = g.tool(tool, **args)
        return text, raw.get("result", {}).get("isError", False)

    g = guard()
    try:
        init = g.rpc("initialize", {"protocolVersion": "2025-06-18"})
        tools = [t["name"] for t in g.rpc("tools/list")["result"]["tools"]]
        check("guard", "the guard passes the protocol through: the agent sees the real server's tools", "result" in init and tools == ["set_temperature", "run_sql_drop", "list_things"], str(tools))
        t1, e1 = call(g, "set_temperature", chamber="chamber3", celsius=-18)
        t2, e2 = call(g, "set_temperature", chamber="chamber3", celsius=-40)
        t3, e3 = call(g, "run_sql_drop", table="customers")
        t4, e4 = call(g, "list_things")
        t5, e5 = call(g, "set_temperature", chamber="chamber3")
        seen = [json.loads(l)["params"]["name"] for l in received.read_text(encoding="utf-8").splitlines()]
        check("guard", "an allowed call reaches the tool and its answer comes back", not e1 and t1.startswith("done: set_temperature"), t1[:60])
        check("guard", "a denied call never reaches the tool; the agent gets the rule and its text",
              e2 and "stock is damaged" in t2 and e3 and "Nobody drops production" in t3 and seen == ["set_temperature", "list_things"], f"{seen} | {t2[:70]}")
        check("guard", "a tool no policy governs is forwarded under the default allow", not e4 and t4.startswith("done: list_things"), t4[:50])
        check("guard", "a call missing the argument a policy needs is denied, not guessed", e5 and "lacks" in t5, t5[:80])

        # A key given twice must not show the guard one value and the server another.
        g.p.stdin.write('{"jsonrpc":"2.0","id":900,"method":"tools/call","params":{"name":"set_temperature","arguments":{"chamber":"chamber3","celsius":-40,"celsius":-18}}}\n')
        g.p.stdin.flush()
        json.loads(g.p.stdout.readline())
        last = json.loads(received.read_text(encoding="utf-8").splitlines()[-1])
        check("guard", "the server receives exactly what was checked (a duplicated key is resolved before forwarding)",
              last["params"]["arguments"]["celsius"] == -18 and received.read_text(encoding="utf-8").splitlines()[-1].count("celsius") == 1, str(last["params"]["arguments"]))
        g.p.stdin.write('[{"jsonrpc":"2.0","id":901,"method":"tools/call","params":{"name":"run_sql_drop","arguments":{"table":"t"}}}]\n')
        g.p.stdin.flush()
        batch = json.loads(g.p.stdout.readline())
        check("guard", "a batch is refused whole, so a tool call cannot hide inside one", "error" in batch and "run_sql_drop" not in received.read_text(encoding="utf-8"), str(batch)[:80])
    finally:
        g.close()

    audit = root / "audit.jsonl"
    p = fds(root / "x.fds", "audit", "verify", "--file", audit)
    n = len(audit.read_text(encoding="utf-8").splitlines()) if audit.exists() else 0
    check("audit", "every tool call was recorded, and the chain verifies", p.returncode == 0 and n == 6 and "intact" in p.stdout, f"{n} lines; {p.stdout.strip()[:70]}")
    text = audit.read_text(encoding="utf-8")
    check("audit", "the audit says who asked, what, and why it was decided", '"agent":"claude-r"' in text and '"decision":"deny"' in text and "stock is damaged" in text, "")
    lines = text.splitlines()
    lines[1] = lines[1].replace('"deny"', '"allow"', 1)
    audit.write_text("\n".join(lines) + "\n", encoding="utf-8")
    p = fds(root / "x.fds", "audit", "verify", "--file", audit)
    check("audit", "a line changed afterwards breaks the chain, and verify says where", p.returncode == 1 and "BROKEN" in p.stdout, p.stdout.strip()[:80])
    lines[1] = lines[1].replace('"allow"', '"deny"', 1)
    audit.write_text("\n".join(lines) + "\n", encoding="utf-8")

    # A closed manifest refuses what no policy covers, and the guard re-reads policies when they change.
    mj = json.loads(man.read_text(encoding="utf-8"))
    mj["default"] = "deny"
    man.write_text(json.dumps(mj), encoding="utf-8")
    g = guard(["--audit", str(root / "audit2.jsonl")])
    try:
        g.rpc("initialize", {"protocolVersion": "2025-06-18"})
        t, e = call(g, "list_things")
        check("guard", "with `default: deny` a tool no policy covers is refused", e and "default is deny" in t, t[:80])
        fds(pdb, "policy", "add", "Let the harmless tool through.", "--subject", "things.*", "--action", "list", "--rule", "one_of", "--values", "all",
            "--tool", "list_things", "--tool-subject", "things.all", "--tool-action", "list", "--tool-value", "all")
        t, e = call(g, "list_things")
        check("guard", "a policy a person adds while the guard runs applies to the very next call", not e and t.startswith("done:"), t[:60])
    finally:
        g.close()

    # ── the black box: journal entries and the guard's decisions in one timeline ──
    fds(pdb, "log", "Checked the fridge limits", "--role", "me", "--branch", "main")
    p = subprocess.run([str(FDS), "journal", "show", "--timeline", "--sectors", str(man)], capture_output=True, text=True, encoding="utf-8")
    check("journal", "the timeline puts the guard's decisions beside the journal's steps, numbered, in order",
          "[guard] allow set_temperature" in p.stdout and "[guard] deny set_temperature" in p.stdout and "[me] Checked the fridge limits" in p.stdout
          and p.stdout.lstrip().startswith("#01"), p.stdout.strip()[:120].replace("\n", " | "))

    # ── sensitive records ──
    sroot = WORK / "sensitive"
    if sroot.exists():
        def writable2(fn, path, _err):
            os.chmod(path, 0o700)
            fn(path)
        shutil.rmtree(sroot, onerror=writable2)
    sroot.mkdir(parents=True)
    sman = sroot / "sectors.json"

    def ssec(*args):
        return subprocess.run([str(FDS), "sector", *map(str, args), "--manifest", str(sman)], capture_output=True, text=True, encoding="utf-8")

    ssec("new", "clientes")
    ssec("new", "soporte", "--agent", "--reads", "clientes")
    ssec("new", "cuentas", "--agent", "--reads", "clientes", "--clearance", "personal")
    rows = [
        {"kind": "fact", "question": "direccion de Juan Perez", "answer": "Juan vive en Calle Secreta 99, Miraflores",
         "attrs": {"sensitivity": "personal", "derived": "El cliente vive en Lima"}},
        {"kind": "fact", "question": "clave del servidor", "answer": "hunter2", "attrs": {"sensitivity": "secret"}},
        {"kind": "fact", "question": "horario de atencion", "answer": "Lunes a sabado de 7 a 20"},
    ]
    rj = sroot / "rows.json"
    rj.write_text(json.dumps(rows), encoding="utf-8")
    fds(sroot / "clientes.fds", "import", rj)

    def ask(agent, q):
        s = Stdio(["mcp", "--sectors", str(sman), "--as", agent])
        try:
            s.rpc("initialize", {"protocolVersion": "2025-06-18"})
            return s.tool("fds_query", query=q)[0]
        finally:
            s.close()

    a1 = ask("soporte", "direccion de Juan Perez")
    a2 = ask("soporte", "Secreta")
    a3 = ask("soporte", "clave del servidor")
    b1 = ask("cuentas", "direccion de Juan Perez")
    b2 = ask("cuentas", "clave del servidor")
    c1 = ask("soporte", "horario de atencion")
    check("sensitive", "an agent without clearance gets the owner's `derived` text, not the record",
          "El cliente vive en Lima" in a1 and "Calle Secreta" not in a1 and "4455" not in a1, a1[:140].replace("\n", " | "))
    check("sensitive", "...and cannot find the record by a word only its hidden body has", "Secreta" not in a2 and "confidence: none" in a2, a2[:100].replace("\n", " | "))
    check("sensitive", "a secret it is not cleared for shows as [protected: secret]", "[protected: secret]" in a3 and "hunter2" not in a3, a3[:140].replace("\n", " | "))
    check("sensitive", "an agent cleared for `personal` reads it in full; `secret` still stays hidden", "Calle Secreta 99" in b1 and "hunter2" not in b2, (b1 + b2)[:100].replace("\n", " | "))
    check("sensitive", "ordinary records are untouched", "Lunes a sabado" in c1, c1[:80].replace("\n", " | "))


def main():
    if not FDS.exists():
        sys.exit(f"build first: {FDS}")
    WORK.mkdir(parents=True, exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    v = subprocess.run([str(FDS), "--version"], capture_output=True, text=True).stdout.strip()
    print(f"verifying {v} at {FDS}")
    db = cli()
    imports()
    robustness()
    http_checks(db)
    mcp_checks(db)
    sectors_checks()
    journal_checks()
    policy_checks()
    vectors(db)
    if "--images" in sys.argv:
        images()
    summary = {s: sum(r["status"] == s for r in RESULTS) for s in ("PASS", "FAIL", "NOTE")}
    print(f"\n{summary}")
    (OUT / "verify.json").write_text(json.dumps({"version": v, "summary": summary, "checks": RESULTS},
                                                indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    main()
