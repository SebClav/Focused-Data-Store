"""What sectors cost in memory: N agents sharing one large base.

  A  every agent runs its own `fds mcp --sectors --as <agent>`: each process
     loads the shared base plus its own sector
  B  one `fds serve --sectors` loads every file once; each agent runs a
     `fds mcp --connect` bridge that holds no data

Each agent asks the same questions; the script records total resident memory
and per-query latency. Results go to research/results/sectors_ram.json.

    python research/sectors_ram.py [--agents 5] [--shared 10000]
"""

import json, os, random, shutil, socket, statistics, subprocess, sys, time, urllib.request
from pathlib import Path

import psutil

ROOT = Path(__file__).resolve().parent.parent
FDS = ROOT / "target" / "release" / ("fds.exe" if os.name == "nt" else "fds")
WORK = ROOT / "research" / "work" / "sectors_ram"
OUT = ROOT / "research" / "results"

rng = random.Random(5)
VOCAB = ("router printer invoice refund shipping warehouse password account policy service server backup "
         "driver contract schedule report payment discount customer order ticket incident update license").split()


def words(n):
    return " ".join(rng.choices(VOCAB, k=n))


def sec(man, *args, json_out=False):
    cmd = [str(FDS)] + (["--json"] if json_out else []) + ["sector", *map(str, args), "--manifest", str(man)]
    p = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8")
    assert p.returncode == 0, p.stderr
    return p


class Agent:
    def __init__(self, args, env=None):
        self.p = psutil.Popen([str(FDS)] + args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, text=True, encoding="utf-8", env=env)
        self.n = 0

    def rpc(self, method, params=None):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}) + "\n")
        self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())

    def ask(self, q):
        t0 = time.perf_counter()
        self.rpc("tools/call", {"name": "fds_query", "arguments": {"query": q}})
        return time.perf_counter() - t0

    def rss(self):
        return self.p.memory_info().rss

    def close(self):
        self.p.kill()


def main():
    n_agents = int(sys.argv[sys.argv.index("--agents") + 1]) if "--agents" in sys.argv else 5
    n_shared = int(sys.argv[sys.argv.index("--shared") + 1]) if "--shared" in sys.argv else 10000
    if WORK.exists():
        shutil.rmtree(WORK)
    WORK.mkdir(parents=True)
    OUT.mkdir(parents=True, exist_ok=True)
    man = WORK / "sectors.json"

    sec(man, "new", "shared")
    rows = WORK / "shared.jsonl"
    questions = [f"{words(rng.randint(4, 7))} {i}" for i in range(n_shared)]
    rows.write_text("\n".join(json.dumps({"kind": "fact", "question": q, "answer": words(rng.randint(30, 90))})
                              for q in questions), encoding="utf-8")
    subprocess.run([str(FDS), "--db", str(WORK / "shared.fds"), "import", str(rows)], check=True, capture_output=True)
    keys = {}
    for i in range(n_agents):
        name = f"agent{i}"
        keys[name] = json.loads(sec(man, "new", name, "--agent", "--reads", "shared", json_out=True).stdout)["key"]
        own = WORK / f"{name}.jsonl"
        own.write_text("\n".join(json.dumps({"kind": "fact", "question": f"{name} note {j} {words(4)}", "answer": words(20)})
                                 for j in range(50)), encoding="utf-8")
        subprocess.run([str(FDS), "--db", str(WORK / f"{name}.fds"), "import", str(own)], check=True, capture_output=True)
    shared_mb = (WORK / "shared.fds").stat().st_size / 2**20
    qs = [rng.choice(questions) for _ in range(10)] + [words(3) for _ in range(10)]

    # A: one full process per agent.
    agents = [Agent(["mcp", "--sectors", str(man), "--as", f"agent{i}"]) for i in range(n_agents)]
    lat_a = []
    for a in agents:
        a.rpc("initialize", {"protocolVersion": "2025-06-18"})
        lat_a += [a.ask(q) for q in qs]
    mem_a = sum(a.rss() for a in agents)
    for a in agents:
        a.close()

    # B: one server, a data-free bridge per agent.
    s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
    srv = psutil.Popen([str(FDS), "serve", "--sectors", str(man), "--port", str(port)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(300):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/", timeout=1); break
        except Exception:
            time.sleep(0.1)
    url = f"http://127.0.0.1:{port}/mcp"
    bridges = [Agent(["mcp", "--connect", url], env=dict(os.environ, FDS_KEY=keys[f"agent{i}"])) for i in range(n_agents)]
    lat_b = []
    for b in bridges:
        b.rpc("initialize", {"protocolVersion": "2025-06-18"})
        lat_b += [b.ask(q) for q in qs]
    mem_server = srv.memory_info().rss
    mem_bridges = sum(b.rss() for b in bridges)
    for b in bridges:
        b.close()
    srv.kill()

    row = {
        "agents": n_agents, "shared_records": n_shared, "shared_file_mb": round(shared_mb, 1),
        "A_processes_mb": round(mem_a / 2**20, 1),
        "B_server_mb": round(mem_server / 2**20, 1), "B_bridges_mb": round(mem_bridges / 2**20, 1),
        "B_total_mb": round((mem_server + mem_bridges) / 2**20, 1),
        "A_query_p50_ms": round(1000 * statistics.median(lat_a), 1),
        "B_query_p50_ms": round(1000 * statistics.median(lat_b), 1),
    }
    print(json.dumps(row, indent=2))
    (OUT / "sectors_ram.json").write_text(json.dumps(row, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
