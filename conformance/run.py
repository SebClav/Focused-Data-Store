"""Runs the conformance suite against the Python implementation.

    python conformance/run.py

The Rust implementation runs the same files:

    cargo test -p fds --test conformance
    cargo test -p fds-cli normalization_vectors

To test another implementation, make it do for every file in `cases/` what
this script does (it is short): read the `.fds`, compare with the `.json`
beside it. Nothing here is specific to Python or to Rust.

What a case file means is in README.md.
"""

import json
import os
import sys

if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import fds  # noqa: E402
import normalize  # noqa: E402
import policy  # noqa: E402

CASES = os.path.join(HERE, "cases")
failures = []
passed = 0


def check(name, ok, detail=""):
    global passed
    if ok:
        passed += 1
    else:
        failures.append(f"{name}: {detail}")
        print(f"  [FAIL] {name} {detail}")


def load(group, name):
    with open(os.path.join(CASES, group, name + ".fds"), "rb") as f:
        data = f.read()
    with open(os.path.join(CASES, group, name + ".json"), encoding="utf-8") as f:
        return data, json.load(f)


def run_valid(name):
    data, exp = load("valid", name)
    try:
        db = fds.Database(data)
        meta, records = db.meta, db.records()
    except fds.FdsError as e:
        check(f"valid/{name}", False, f"refused: {e}")
        return
    check(f"valid/{name} meta", meta == exp["meta"], f"{meta} != {exp['meta']}")
    check(f"valid/{name} records", records == exp["records"], "records differ")
    check(f"valid/{name} minor", db.minor == exp["minor"], f"{db.minor} != {exp['minor']}")
    if exp["reproducible"]:
        again = fds.encode(meta, records)
        check(f"valid/{name} byte-identical rewrite", again == data, "re-encoding changed the bytes")


def run_invalid(name):
    data, exp = load("invalid", name)
    stage = exp["stage"]
    try:
        db = fds.Database(data)
    except fds.FdsError as e:
        check(f"invalid/{name}", stage == "open" and e.category == exp["error"], f"got {e.category} at open, want {exp['error']} at {stage}")
        return
    if stage == "open":
        check(f"invalid/{name}", False, f"opened, but must fail with {exp['error']}")
        return
    readable, first_error = [], None
    for i in range(db.count):
        try:
            db.get(i)
            readable.append(i)
        except fds.FdsError as e:
            first_error = first_error or e.category
    check(f"invalid/{name}", first_error == exp["error"] and readable == exp["readable"],
          f"error {first_error} readable {readable}, want {exp['error']} {exp['readable']}")


def run_truncation():
    """A good file cut at every length is `truncated` (or, if cut inside the
    magic, still a prefix of it). Never a crash, never a wrong answer."""
    data, _ = load("valid", "all_fields")
    for cut in range(len(data)):
        try:
            fds.Database(data[:cut]).records()
            check(f"truncation at {cut}", False, "opened")
            return
        except fds.FdsError as e:
            if e.category != "truncated":
                check(f"truncation at {cut}", False, f"category {e.category}")
                return
    check("truncation at every length", True)


def run_hostile_text():
    """Odd and large inputs: no exception, the expected size."""
    cases = {
        "lone marks": ("\u0301\u0301\u093e", []),
        "controls and noncharacters": ("\x00\ufffe\uffff\ufffd\U0010ffff\ue000", []),
        "lone surrogates": ("\ud800 \udfff", []),
        "emoji sequences": ("\U0001f468\u200d\U0001f469\u200d\U0001f467 \U0001f1e7\U0001f1f4", []),
        "long run": ("a" * 200000, ["a" * 200000]),
        "many marks": ("e" + "\u0301\u0323" * 200000, ["e"]),
        "long unspaced run": ("\u6f22" * 50000, None),
    }
    for name, (text, want) in cases.items():
        try:
            got = normalize.search_terms(text)
            ok = len(got) == 2 * 50000 - 1 if want is None else got == want
            check(f"hostile input: {name}", ok, f"{len(got)} terms")
        except Exception as e:  # noqa: BLE001
            check(f"hostile input: {name}", False, repr(e))
    every = "".join(chr(c) for c in range(0x30000) if not 0xD800 <= c < 0xE000)
    try:
        normalize.search_terms(every)
        normalize.search_terms(every[::-1])
        check("hostile input: every code point", True)
    except Exception as e:  # noqa: BLE001
        check("hostile input: every code point", False, repr(e))


def run_normalization():
    with open(os.path.join(HERE, "normalization.json"), encoding="utf-8") as f:
        v = json.load(f)
    for c in v["normalize"]:
        got = (normalize.canonical(c["input"]), normalize.content_tokens(c["input"]), normalize.token_set_key(c["input"]))
        check(f"normalize {c['input']!r}", got == (c["canonical"], c["tokens"], c["key"]), str(got))
    for a, b in v["same_key"]:
        check(f"same key {a!r} {b!r}", normalize.token_set_key(a) == normalize.token_set_key(b))
    for a, b in v["different_key"]:
        check(f"different key {a!r} {b!r}", normalize.token_set_key(a) != normalize.token_set_key(b))
    for c in v["search_terms"]:
        check(f"search terms of {c['input']!r}", normalize.search_terms(c["input"]) == c["terms"], str(normalize.search_terms(c["input"])))
    for a, b in v["same_terms"]:
        check(f"same terms {a!r} {b!r}", normalize.search_terms(a) == normalize.search_terms(b))
    for a, b in v["different_terms"]:
        check(f"different terms {a!r} {b!r}", normalize.search_terms(a) != normalize.search_terms(b))
    for c in v["search_fold"]:
        check(f"search fold of {c['input']!r}", normalize.search_fold(c["input"]) == c["folded"], normalize.search_fold(c["input"]))
    for c in v["matches"]:
        q, t = normalize.search_terms(c["query"]), set(normalize.search_terms(c["text"]))
        got = bool(q) and all(x in t for x in q)
        check(f"match {c['query']!r} in {c['text']!r}", got == c["match"], str(got))
    run_hostile_text()
    for c in v["ids"]:
        check(f"id of {c['input']!r}", normalize.stable_id(c["input"]) == c["id"], normalize.stable_id(c["input"]))


def run_policy():
    with open(os.path.join(HERE, "policy.json"), encoding="utf-8") as f:
        v = json.load(f)
    pols = v["policies"]
    for c in v["checks"]:
        got = policy.check(pols, c["subject"], c["action"], c.get("value"), c.get("default", "allow"))
        want = {"decision": c["decision"], "default_applied": c["default_applied"], "because": sorted(c["because"])}
        label = f"check {c['action']} {c['subject']} = {c.get('value')!r}" + (f" (default {c['default']})" if "default" in c else "")
        check(label, got == want, f"got {got}, want {want}")
    for c in v["fill"]:
        check(f"fill {c['template']!r} {c['args']}", policy.fill(c["template"], c["args"]) == c["expected"], str(policy.fill(c["template"], c["args"])))
    red = v["redact"]
    for c in red["cases"]:
        out = {r["name"]: r for r in policy.redact(red["records"], c["clearance"])}
        check(f"redact with clearance {c['clearance']}", {k: r["body"] for k, r in out.items()} == c["bodies"], str({k: r["body"] for k, r in out.items()}))
        hidden = [r for r in out.values() if r.get("attrs", {}).get("protected")]
        check(f"redact {c['clearance']}: a stand-in keeps only its level, and loses source, links and vectors",
              all(set(r["attrs"]) == {"sensitivity", "protected"} and r["source"] == red["stand_in_source"] and r["links"] == [] and r["embedding"] is None for r in hidden))


def run_audit():
    base = os.path.join(CASES, "audit")
    with open(os.path.join(base, "expected.json"), encoding="utf-8") as f:
        exp = json.load(f)
    for c in exp["cases"]:
        with open(os.path.join(base, c["file"]), encoding="utf-8") as f:
            broken, head = policy.verify_audit(f.read())
        check(f"audit {c['file']}", (broken is None) == c["intact"] and broken == c["broken_at"], f"broken at {broken}, want {c['broken_at']}")
        if c["intact"]:
            check("audit head hash of the good chain", head == exp["head_of_good"], head)


def main():
    with open(os.path.join(CASES, "MANIFEST.json"), encoding="utf-8") as f:
        manifest = json.load(f)
    for name in manifest["valid"]:
        run_valid(name)
    for name in manifest["invalid"]:
        run_invalid(name)
    run_truncation()
    run_normalization()
    run_policy()
    run_audit()
    print(f"{passed} checks passed, {len(failures)} failed "
          f"({len(manifest['valid'])} valid files, {len(manifest['invalid'])} damaged files)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
