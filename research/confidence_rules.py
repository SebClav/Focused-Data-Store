"""What if `strong` looked only at the first result?

Today (query.rs §5) a hybrid answer is `strong` when ANY returned record was
found by both retrievers, or ANY record contains every query word. This
replays every hybrid query of research/accuracy.py, keeps the per-record
features FDS already exports (bm25, cosine, match), and scores candidate rules
on the same questions without touching the engine:

  current      the rule as shipped (read from FDS's own answer)
  top1-both    strong only if the FIRST record was found by both retrievers
  top1-cover   strong only if the first record contains every query word
  top1-both+cover  both conditions on the first record
  keyword      what keyword-only mode says (no vectors), for reference

Needs the databases research/accuracy.py built and embedded.
"""

import json, os, re, subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FDS = ROOT / "target" / "release" / ("fds.exe" if os.name == "nt" else "fds")
WORK = ROOT / "research" / "work" / "accuracy"
sys_path = ROOT / "research"
import sys
sys.path.insert(0, str(sys_path))
from accuracy import load_domains, norm  # noqa: E402

STOP = None


def content_tokens(db, text):
    return set(norm(text).split())


def main():
    out = {}
    for dom in load_domains():
        db = WORK / f"{dom['name']}.fds"
        rows = []
        items = [(q, True) for q in dom["queries"]] + [({"q": o["q"], "gold": None, "kind": o["kind"]}, False) for o in dom["ood"]]
        for item, in_domain in items:
            p = subprocess.run([str(FDS), "--db", str(db), "--json", "query", item["q"]], capture_output=True, text=True, encoding="utf-8")
            r = json.loads(p.stdout)
            recs = r.get("records", [])
            top = recs[0] if recs else {}
            rows.append({
                "in": in_domain, "kind": item.get("kind"), "tier": item.get("tier"),
                "conf": r.get("confidence"),
                "right": bool(recs) and in_domain and norm(top.get("question") or top.get("title")) == item["gold"],
                "top_match": top.get("match"), "top_bm25": top.get("bm25", 0), "top_cos": top.get("cosine", 0),
                "top_body": (top.get("question") or "") + " " + (top.get("title") or "") + " " + (top.get("body") or ""),
                "q": item["q"],
            })
        rules = {
            "current": lambda x: x["conf"] in ("exact", "strong"),
            "top1-both": lambda x: x["conf"] == "exact" or (x["top_bm25"] > 0 and x["top_cos"] > 0),
            "top1-cover": lambda x: x["conf"] == "exact" or (x["conf"] != "none" and set(norm(x["q"]).split()) - STOPWORDS <= set(norm(x["top_body"]).split())),
            "top1-both+cover": lambda x: x["conf"] == "exact" or (x["top_bm25"] > 0 and x["top_cos"] > 0
                                                                   and set(norm(x["q"]).split()) - STOPWORDS <= set(norm(x["top_body"]).split())),
        }
        res = {}
        ind = [x for x in rows if x["in"]]
        near = [x for x in rows if not x["in"] and x["kind"] == "near"]
        far = [x for x in rows if not x["in"] and x["kind"] == "far"]
        for name, rule in rules.items():
            d = [x for x in ind if rule(x)]
            res[name] = {
                "direct": round(100 * len(d) / len(ind), 1),
                "precision": round(100 * sum(x["right"] for x in d) / len(d), 1) if d else None,
                "ood_near_direct": round(100 * sum(rule(x) for x in near) / len(near), 1) if near else None,
                "ood_far_direct": round(100 * sum(rule(x) for x in far) / len(far), 1) if far else None,
            }
        out[dom["name"]] = res
        for name, v in res.items():
            print(f"{dom['name']:<12} {name:<16} direct {v['direct']:>5}  precision {v['precision']}  "
                  f"ood-near direct {v['ood_near_direct']}  ood-far direct {v['ood_far_direct']}", flush=True)
    (ROOT / "research" / "results" / "confidence_rules.json").write_text(json.dumps(out, indent=2), encoding="utf-8")


# Function words of both languages, as in fds-cli/src/text.rs.
STOPWORDS = set("""a about above after again against all am an and any are as at be because been before being below
between both but by can cannot could did do does doing down during each few for from further had has have having he
her here hers herself him himself his how i if in into is it its itself just me more most my myself no nor not now of
off on once only or other our ours ourselves out over own same she should so some such than that the their theirs them
themselves then there these they this those through to too under until up very was we were what when where which while
who whom why will with would you your yours yourself yourselves el la los las un una unos unas de del al y o u que en es
son por para con sin se su sus lo le les como cómo qué cuál cuáles cuánto cuánta cuántos cuántas cuando cuándo donde
dónde quién quiénes este esta estos estas ese esa esos esas hay ser está están fue era mi mis tu tus me te nos ya muy más
pero si también entre sobre s""".split())

if __name__ == "__main__":
    main()
