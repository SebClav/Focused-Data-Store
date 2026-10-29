"""Answer quality across domains, keyword-only and hybrid (keywords + vectors).

For each domain in research/domains/*.json, plus the existing winops set, it
builds a fresh database, asks every query through the real CLI and scores:

  hit@1, hit@5, MRR         is the right record first / in the top five
  direct                    share of in-domain queries FDS answers with
                            confidence exact or strong (safe to act on)
  direct precision          of those, how many are right
  false none                in-domain queries that got `none`
  OOD rejected              out-of-domain queries that got `none`
  OOD direct (danger)       out-of-domain queries answered exact/strong
  tokens                    what FDS hands a model, against the whole base

    python research/accuracy.py [--no-vectors]
"""

import json, os, re, statistics, subprocess, sys, time, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FDS = ROOT / "target" / "release" / ("fds.exe" if os.name == "nt" else "fds")
WORK = ROOT / "research" / "work" / "accuracy"
OUT = ROOT / "research" / "results"
EMBED_MODEL = "all-minilm"


def norm(s):
    """FDS stores a question in canonical form (lowercase, punctuation to
    spaces), so gold and answers are compared the same way."""
    return " ".join(re.sub(r"[^\w]+", " ", (s or "").lower()).split())


def load_domains():
    doms = []
    for f in sorted((ROOT / "research" / "domains").glob("*.json")):
        d = json.loads(f.read_text(encoding="utf-8"))
        recs = []
        for r in d["records"]:
            if r["kind"] == "procedure":
                recs.append({"kind": "procedure", "intent": r["intent"],
                             "steps": [{"action": "run", "command": r["command"]}]})
            else:
                recs.append(r)
        keys = [norm(r.get("question") or r.get("intent")) for r in recs]
        qs = [{"q": q, "gold": keys[i], "tier": t} for i, t, q in d["queries"]]
        ood = [{"q": q, "kind": k} for k, q in d["ood"]]
        cross = [{"q": q, "gold": keys[i], "tier": "cross"} for i, q in d.get("cross", [])]
        doms.append({"name": d["domain"], "lang": d["lang"], "records": recs, "queries": qs, "ood": ood, "cross": cross})
    # The original single-domain benchmark, same scoring.
    wq = json.loads((ROOT / "winops" / "queries.json").read_text(encoding="utf-8"))
    wc = json.loads((ROOT / "winops" / "commands.json").read_text(encoding="utf-8"))
    doms.append({"name": "winops", "lang": "en",
                 "records": [{"kind": "procedure", **c} for c in wc],
                 "queries": [{"q": x["q"], "gold": norm(x["intent"]), "tier": 0} for x in wq if x.get("intent")],
                 "ood": [{"q": x["q"], "kind": "far"} for x in wq if not x.get("intent")], "cross": []})
    return doms


def build(dom):
    db = WORK / f"{dom['name']}.fds"
    for p in (db, db.with_suffix(".fds.tmp")):
        if p.exists():
            p.unlink()
    subprocess.run([str(FDS), "--db", str(db), "init", dom["name"]], check=True, capture_output=True)
    rows = WORK / f"{dom['name']}.jsonl"
    rows.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in dom["records"]), encoding="utf-8")
    p = subprocess.run([str(FDS), "--db", str(db), "import", str(rows)], capture_output=True, text=True, encoding="utf-8")
    assert p.returncode == 0, p.stderr
    return db


def ask(db, q):
    p = subprocess.run([str(FDS), "--db", str(db), "--json", "query", q], capture_output=True, text=True, encoding="utf-8")
    try:
        r = json.loads(p.stdout)
    except json.JSONDecodeError:
        return {"confidence": "error", "records": [], "used_tokens": 0}
    return r


def rank_of(r, gold):
    for i, rec in enumerate(r.get("records", [])):
        if norm(rec.get("question") or rec.get("title")) == gold:
            return i + 1
    return None


def score(db, dom):
    per_q = []
    for item in dom["queries"] + dom["cross"]:
        r = ask(db, item["q"])
        k = rank_of(r, item["gold"])
        per_q.append({**item, "confidence": r.get("confidence"), "rank": k, "tokens": r.get("used_tokens", 0),
                      "top": norm((r.get("records") or [{}])[0].get("question"))})
    per_o = []
    for item in dom["ood"]:
        r = ask(db, item["q"])
        per_o.append({**item, "confidence": r.get("confidence"), "tokens": r.get("used_tokens", 0),
                      "top": norm((r.get("records") or [{}])[0].get("question"))})
    return per_q, per_o


def summarize(per_q, per_o, tiers=None):
    qs = [x for x in per_q if tiers is None or x["tier"] in tiers]
    if not qs:
        return None
    n = len(qs)
    direct = [x for x in qs if x["confidence"] in ("exact", "strong")]
    s = {
        "n": n,
        "hit1": round(100 * sum(x["rank"] == 1 for x in qs) / n, 1),
        "hit5": round(100 * sum(x["rank"] is not None and x["rank"] <= 5 for x in qs) / n, 1),
        "mrr": round(sum(1 / x["rank"] for x in qs if x["rank"]) / n, 3),
        "direct": round(100 * len(direct) / n, 1),
        "direct_precision": round(100 * sum(x["rank"] == 1 for x in direct) / len(direct), 1) if direct else None,
        "false_none": round(100 * sum(x["confidence"] == "none" for x in qs) / n, 1),
        "tokens_mean": round(statistics.mean(x["tokens"] for x in qs), 1),
    }
    if per_o and tiers is None:
        s["ood_n"] = len(per_o)
        for kind in ("near", "far"):
            o = [x for x in per_o if x["kind"] == kind]
            if o:
                s[f"ood_{kind}_rejected"] = round(100 * sum(x["confidence"] == "none" for x in o) / len(o), 1)
                s[f"ood_{kind}_direct"] = round(100 * sum(x["confidence"] in ("exact", "strong") for x in o) / len(o), 1)
    return s


def whole_base_tokens(db):
    p = subprocess.run([str(FDS), "--db", str(db), "--json", "ls"], capture_output=True, text=True, encoding="utf-8")
    rows = json.loads(p.stdout)
    rows = rows if isinstance(rows, list) else rows.get("rows", rows.get("records", []))
    return sum(r.get("tokens", 0) for r in rows)


def ollama_up():
    try:
        urllib.request.urlopen("http://127.0.0.1:11434/api/tags", timeout=2); return True
    except Exception:
        return False


def main():
    WORK.mkdir(parents=True, exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)
    use_vectors = "--no-vectors" not in sys.argv and ollama_up()
    report = {"embed_model": EMBED_MODEL if use_vectors else None, "domains": {}}
    for dom in load_domains():
        db = build(dom)
        entry = {"lang": dom["lang"], "records": len(dom["records"]), "base_tokens": whole_base_tokens(db), "modes": {}}
        modes = ["keyword"] + (["hybrid"] if use_vectors else [])
        for mode in modes:
            if mode == "hybrid":
                subprocess.run([str(FDS), "--db", str(db), "embed", "--model", EMBED_MODEL], capture_output=True, timeout=1800)
            t0 = time.time()
            per_q, per_o = score(db, dom)
            m = {"all": summarize([x for x in per_q if x["tier"] != "cross"], per_o)}
            for t in (1, 2, 3):
                m[f"tier{t}"] = summarize(per_q, [], {t})
            if dom["cross"]:
                m["cross_lingual"] = summarize(per_q, [], {"cross"})
            m["seconds"] = round(time.time() - t0, 1)
            m["misses"] = [{"q": x["q"], "tier": x["tier"], "got": x["top"], "conf": x["confidence"]}
                           for x in per_q if x["rank"] != 1][:60]
            m["ood_answers"] = [{"q": x["q"], "kind": x["kind"], "conf": x["confidence"], "got": x["top"]} for x in per_o]
            entry["modes"][mode] = m
            a = m["all"]
            print(f"{dom['name']:<12} {mode:<8} hit@1 {a['hit1']:>5}  hit@5 {a['hit5']:>5}  direct {a['direct']:>5} "
                  f"(prec {a['direct_precision']})  none-in {a['false_none']:>5}  "
                  f"ood-near rej {a.get('ood_near_rejected', '-')}/direct {a.get('ood_near_direct', '-')}  "
                  f"ood-far rej {a.get('ood_far_rejected', '-')}  tok {a['tokens_mean']}/{entry['base_tokens']}", flush=True)
        report["domains"][dom["name"]] = entry
    (OUT / "accuracy.json").write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    main()
