"""How FDS behaves as a database grows: 1k, 10k and 100k records.

For each size it builds a database the way a user would (`import` for facts
and procedures, `add` for documents), then measures:

  build        time to load everything
  size         bytes on disk, bytes per record
  cold query   one CLI process per query (start, open, answer, exit), p50/p95
  warm query   the HTTP server, loaded once, p50/p95
  one write    adding a single fact to the full database (the file is rewritten)
  memory       peak working set of the CLI query and of the server

The text is synthetic but shaped like real data: words drawn with a Zipf law
from a vocabulary of technical and everyday terms, questions of 4-9 words,
answers and fragments of 40-160 words.

    python research/scale.py [--sizes 1000,10000,100000] [--vectors 10000]
"""

import json, os, random, shutil, socket, statistics, subprocess, sys, time, urllib.request
from pathlib import Path
import psutil

ROOT = Path(__file__).resolve().parent.parent
FDS = ROOT / "target" / "release" / ("fds.exe" if os.name == "nt" else "fds")
WORK = ROOT / "research" / "work" / "scale"
OUT = ROOT / "research" / "results"

rng = random.Random(7)
BASE = """network router switch firewall port address gateway dns cache server client printer driver
spooler queue disk partition volume file folder backup restore archive zip password account user group
permission policy service process memory cpu battery power voltage current resistor capacitor diode
transistor regulator sensor board pin clock signal timer register invoice order customer payment refund
shipping warehouse stock product price discount report monthly sales tax receipt contract employee
schedule meeting calendar ticket incident error warning log event update patch install uninstall
configure enable disable restart reset check list show find search export import sync upload download""".split()
# A long tail of rarer words so the vocabulary behaves like real text.
TAIL = [f"{a}{b}" for a in ("net", "sys", "data", "auto", "micro", "multi", "hyper", "power", "cloud", "smart")
        for b in ("link", "port", "core", "base", "flow", "gate", "node", "grid", "mesh", "cast", "loop", "sync")]
VOCAB = BASE + TAIL
WEIGHTS = [1 / (i + 1) ** 0.9 for i in range(len(VOCAB))]


def words(n):
    return " ".join(rng.choices(VOCAB, WEIGHTS, k=n))


def build_inputs(n, folder):
    """10% facts, 10% procedures (one JSONL), 80% document fragments (Markdown,
    100 sections per file, one fragment per section)."""
    folder.mkdir(parents=True, exist_ok=True)
    n_fact, n_proc = n // 10, n // 10
    n_frag = n - n_fact - n_proc
    rows, questions = [], []
    for i in range(n_fact):
        q = f"{words(rng.randint(4, 8))} {i}"
        questions.append(q)
        rows.append({"kind": "fact", "question": q, "answer": words(rng.randint(40, 120))})
    for i in range(n_proc):
        q = f"{words(rng.randint(4, 8))} p{i}"
        questions.append(q)
        rows.append({"kind": "procedure", "intent": q, "steps": [{"action": "run", "command": words(4)}]})
    (folder / "rows.jsonl").write_text("\n".join(json.dumps(r) for r in rows), encoding="utf-8")
    docs = folder / "docs"
    docs.mkdir(exist_ok=True)
    per_file = 100
    for f in range((n_frag + per_file - 1) // per_file):
        secs = min(per_file, n_frag - f * per_file)
        text = "\n\n".join(f"## section {f}-{s} {words(3)}\n\n{words(rng.randint(60, 160))}" for s in range(secs))
        (docs / f"doc{f:05d}.md").write_text(f"# document {f}\n\n{text}\n", encoding="utf-8")
    return questions


def peak_run(cmd):
    """Run a command, sampling its working set; returns (seconds, peak bytes, completed)."""
    # Output goes to a file: a pipe nobody reads fills up and blocks the child.
    sink = WORK / "out.tmp"
    with open(sink, "wb") as fh:
        t0 = time.perf_counter()
        p = psutil.Popen(cmd, stdout=fh, stderr=subprocess.STDOUT)
        peak = 0
        while p.poll() is None:
            try:
                peak = max(peak, p.memory_info().rss)
            except psutil.Error:
                break
            time.sleep(0.002)
        dt = time.perf_counter() - t0
    return dt, peak, (p.returncode, sink.read_bytes(), b"")


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(q * (len(xs) - 1))))]


def http_query(port, q):
    req = urllib.request.Request(f"http://127.0.0.1:{port}/query", data=json.dumps({"q": q}).encode(),
                                 headers={"content-type": "application/json"})
    t0 = time.perf_counter()
    urllib.request.urlopen(req, timeout=60).read()
    return time.perf_counter() - t0


def measure(n, vectors):
    folder = WORK / f"n{n}"
    if folder.exists():
        shutil.rmtree(folder)
    questions = build_inputs(n, folder)
    db = folder / "scale.fds"
    subprocess.run([str(FDS), "--db", str(db), "init", f"scale{n}"], check=True, capture_output=True)

    t0 = time.perf_counter()
    subprocess.run([str(FDS), "--db", str(db), "import", str(folder / "rows.jsonl")], check=True, capture_output=True)
    t_import = time.perf_counter() - t0
    t0 = time.perf_counter()
    subprocess.run([str(FDS), "--db", str(db), "add", str(folder / "docs")], check=True, capture_output=True)
    t_add = time.perf_counter() - t0
    st = json.loads(subprocess.run([str(FDS), "--db", str(db), "--json", "status"], capture_output=True, text=True).stdout)

    # Queries: half exact questions, half keyword searches of 3 words.
    qs = [rng.choice(questions) for _ in range(15)] + [words(3) for _ in range(15)]
    cold, peaks = [], []
    for q in qs:
        dt, peak, _ = peak_run([str(FDS), "--db", str(db), "--json", "query", q])
        cold.append(dt); peaks.append(peak)

    s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
    srv = psutil.Popen([str(FDS), "--db", str(db), "serve", "--port", str(port)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(300):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/status", timeout=1); break
        except Exception:
            time.sleep(0.1)
    warm = [http_query(port, q) for q in qs * 2]
    srv_mem = srv.memory_info().rss
    srv.kill()

    writes = []
    for i in range(3):
        dt, _, _ = peak_run([str(FDS), "--db", str(db), "fact", "add", f"scale probe {i}", "probe answer"])
        writes.append(dt)

    row = {
        "records": st["records"], "facts": st["facts"], "procedures": st["procedures"], "fragments": st["knowledge"],
        "import_s": round(t_import, 2), "add_docs_s": round(t_add, 2),
        "bytes": db.stat().st_size, "bytes_per_record": round(db.stat().st_size / st["records"]),
        "cold_p50_ms": round(1000 * statistics.median(cold), 1), "cold_p95_ms": round(1000 * pct(cold, .95), 1),
        "warm_p50_ms": round(1000 * statistics.median(warm), 2), "warm_p95_ms": round(1000 * pct(warm, .95), 2),
        "cli_peak_mb": round(max(peaks) / 2**20, 1), "server_mb": round(srv_mem / 2**20, 1),
        "one_write_s": round(statistics.median(writes), 2),
    }

    if vectors and n <= vectors:
        t0 = time.perf_counter()
        p = subprocess.run([str(FDS), "--db", str(db), "embed", "--model", "all-minilm"], capture_output=True, text=True, timeout=7200)
        row["embed_s"] = round(time.perf_counter() - t0, 1)
        row["bytes_with_vectors"] = db.stat().st_size
        srv = psutil.Popen([str(FDS), "--db", str(db), "serve", "--port", str(port)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(300):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{port}/status", timeout=1); break
            except Exception:
                time.sleep(0.1)
        vw = [http_query(port, q) for q in qs]
        row["vector_warm_p50_ms"] = round(1000 * statistics.median(vw), 1)
        row["vector_warm_p95_ms"] = round(1000 * pct(vw, .95), 1)
        row["server_mb_with_vectors"] = round(srv.memory_info().rss / 2**20, 1)
        srv.kill()
    print(json.dumps(row), flush=True)
    return row


def main():
    sizes = [1000, 10000, 100000]
    vectors = 10000
    if "--sizes" in sys.argv:
        sizes = [int(x) for x in sys.argv[sys.argv.index("--sizes") + 1].split(",")]
    if "--vectors" in sys.argv:
        vectors = int(sys.argv[sys.argv.index("--vectors") + 1])
    OUT.mkdir(parents=True, exist_ok=True)
    rows = [measure(n, vectors) for n in sizes]
    machine = {"cpu": psutil.cpu_count(logical=True), "ram_gb": round(psutil.virtual_memory().total / 2**30, 1),
               "os": sys.platform}
    (OUT / "scale.json").write_text(json.dumps({"machine": machine, "rows": rows}, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
