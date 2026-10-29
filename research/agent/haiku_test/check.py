"""Scores the files the agents wrote against the café's real data.
For each task: how many required facts are present (exactly), and how many
invented values appear (a price, a time, a colour or a file the database does not have)."""
import re, sys, json
from pathlib import Path

OUT = Path(__file__).parent / (sys.argv[1] if len(sys.argv) > 1 else "out")
PRICES = {"7", "8", "11", "12", "13", "9", "14", "6"}
TIMES = {"7:00", "20:00", "8:00", "14:00"}
ITEMS = ["Espresso", "Americano", "Cappuccino", "Flat white", "Cold brew", "Croissant", "Torta de zanahoria", "Alfajor"]
IMGS = {
    "img/fachada.jpg": "Fachada del Café Aurora con toldo naranja y mesas en la vereda",
    "img/espresso.jpg": "Un espresso recién servido en taza blanca sobre madera clara",
    "img/pasteleria.jpg": "Vitrina de pastelería con croissants, tortas de zanahoria y alfajores",
    "img/equipo.jpg": "Tres baristas del Café Aurora sonriendo detrás de la barra",
}
COLORS = {"#f6efe3", "#3b2417", "#d9772b"}


def read(name):
    p = OUT / name
    return p.read_text(encoding="utf-8", errors="replace") if p.exists() else None


def textos(t):
    need = [("7:00", "7:00"), ("20:00", "20:00"), ("8:00", "8:00"), ("14:00", "14:00"), ("dirección", "Av. La Mar 1250"),
            ("teléfono", "999 123 456")]
    got = sum(1 for _, s in need if s in t)
    got += sum(1 for p in PRICES if re.search(rf"S/\s*{p}\b(?![.,]\d)", t))
    got += sum(1 for i in ITEMS if i.lower() in t.lower())
    total = len(need) + len(PRICES) + len(ITEMS)
    bad = [p for p in re.findall(r"S/\s*(\d+(?:[.,]\d+)?)", t) if p.replace(",", ".").rstrip("0").rstrip(".") not in PRICES]
    bad += [x for x in re.findall(r"\b\d{1,2}:\d{2}\b", t) if x not in TIMES]
    return got, total, bad


def imagenes(t):
    got = 0
    for src, alt in IMGS.items():
        tag = re.search(rf"<img[^>]*src=[\"']{re.escape(src)}[\"'][^>]*>", t)
        if tag:
            got += 1
            got += alt in tag.group(0)
    total = 2 * len(IMGS)
    bad = [s for s in re.findall(r"src=[\"']([^\"']+)[\"']", t) if s not in IMGS]
    return got, total, bad


def marca(t):
    low = t.lower()
    got = sum(1 for c in COLORS if c in low) + ("fraunces" in low) + ("inter" in low)
    bad = [h for h in re.findall(r"#[0-9a-fA-F]{3,8}\b", t) if h.lower() not in COLORS]
    return got, len(COLORS) + 2, bad


TASKS = {"textos": ("html", textos), "imagenes": ("html", imagenes), "marca": ("css", marca)}
rows = {}
for cond in ("A", "B"):
    for task, (ext, fn) in TASKS.items():
        t = read(f"{task}_{cond}.{ext}")
        if t is None:
            rows[(task, cond)] = None
            continue
        rows[(task, cond)] = fn(t)

print(f"{'task':10} {'cond':5} {'correct':>9} {'invented':>9}")
sums = {"A": [0, 0, 0], "B": [0, 0, 0]}
for (task, cond), r in rows.items():
    if r is None:
        print(f"{task:10} {cond:5} {'(no file)':>9}")
        continue
    got, total, bad = r
    sums[cond][0] += got
    sums[cond][1] += total
    sums[cond][2] += len(bad)
    print(f"{task:10} {cond:5} {got:>4}/{total:<4} {len(bad):>9}  {bad if bad else ''}")
for cond, (g, t, b) in sums.items():
    print(f"TOTAL {cond}: {g}/{t} correct, {b} invented")
