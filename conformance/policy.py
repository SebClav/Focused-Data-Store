"""The deterministic parts of the guard (SPEC.md §11), second implementation.

The check, the filling of a guard template from a tool call's arguments, and
the redaction of sensitive records. Written from the spec, not from the Rust
code; standard library only.
"""

import hashlib
import json
import re

# SPEC §11.2: a number is a finite decimal in ASCII: optional sign, digits with
# an optional fraction, an optional exponent. Not inf, NaN, underscores or the
# digits of other alphabets, all of which Python's float() would accept.
_NUMBER = re.compile(r"^[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)([eE][+-]?[0-9]+)?$")


def number(s):
    t = s.strip()
    if not _NUMBER.match(t):
        return None
    v = float(t)
    return v if v == v and v not in (float("inf"), float("-inf")) else None


def _matches_subject(pattern, subject):
    return subject.startswith(pattern[:-1]) if pattern.endswith("*") else pattern == subject


def _matches_action(pattern, action):
    return pattern == "*" or pattern == action


def _violated(p, value):
    """None if the policy is satisfied, else why not. What cannot be evaluated is not satisfied."""
    rule = p.get("rule")
    if rule == "deny":
        return "denied outright"
    if rule == "range":
        lo = number(p["min"]) if p.get("min") is not None else None
        hi = number(p["max"]) if p.get("max") is not None else None
        if lo is None or hi is None:
            return "no usable min and max"
        if value is None:
            return "no value to check"
        n = number(value)
        if n is None:
            return "not a number"
        return None if lo <= n <= hi else "outside the range"
    if rule == "one_of":
        values = [v.strip() for v in (p.get("values") or "").split(",") if v.strip()]
        if not values:
            return "no values listed"
        if value is None:
            return "no value to check"
        return None if value.strip() in values else "not one of the values"
    return "unknown rule"


def check(policies, subject, action, value=None, default="allow"):
    """Every policy a person verified, whose subject and action match, is
    evaluated; one violation denies. With no match the answer is the default."""
    counted = [p for p in policies if p.get("verified_by", "human") == "human"]
    matching = [p for p in counted if _matches_subject(p["subject"], subject) and _matches_action(p.get("action", "*"), action)]
    denying = [p["name"] for p in matching if _violated(p, value) is not None]
    if denying:
        return {"decision": "deny", "default_applied": False, "because": sorted(denying)}
    if not matching:
        return {"decision": "deny" if default == "deny" else "allow", "default_applied": True, "because": []}
    return {"decision": "allow", "default_applied": False, "because": sorted(p["name"] for p in matching)}


def fill(template, args):
    """`{name}` takes the call's argument of that name; a name the call lacks,
    an unclosed brace, or an argument that is not a string, number or boolean
    makes the whole template unusable (None)."""
    out = []
    rest = template
    while "{" in rest:
        i = rest.index("{")
        out.append(rest[:i])
        j = rest.find("}", i)
        if j < 0:
            return None
        name = rest[i + 1 : j]
        v = args.get(name)
        if isinstance(v, str):
            out.append(v)
        elif isinstance(v, (bool, int, float)):
            out.append(json.dumps(v))
        else:
            return None
        rest = rest[j + 1 :]
    out.append(rest)
    return "".join(out)


def verify_audit(text):
    """The guard's audit chain (SPEC §11.4). Returns (broken_at, head): the
    1-based line where the chain first fails (None if it holds) and the SHA-256
    of the last good line."""
    prev, n = "0" * 64, 0
    for i, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            v = json.loads(line)
        except ValueError:
            return i, prev
        n += 1
        if not isinstance(v, dict) or v.get("n") != n or v.get("prev") != prev:
            return i, prev
        prev = hashlib.sha256(line.encode("utf-8")).hexdigest()
    return None, prev


def redact(records, clearance):
    """Records as a caller with this clearance may see them."""
    cleared = {c.strip().lower() for c in clearance}
    out = []
    for r in records:
        level = (r.get("attrs", {}).get("sensitivity") or "").strip()
        if level and level.lower() not in cleared:
            derived = (r["attrs"].get("derived") or "").strip()
            out.append({
                "name": r["name"], "title": r["title"], "body": derived or f"[protected: {level}]",
                "source": "[protected]", "attrs": {"sensitivity": level, "protected": "true"}, "links": [], "embedding": None,
            })
        else:
            out.append(dict(r))
    return out
