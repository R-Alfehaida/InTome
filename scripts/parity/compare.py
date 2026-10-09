"""Compare Prism vs InTome extraction for one document.

  python scripts/parity/compare.py <file> [--min 0.90]

Runs both dumpers, then reports page count, whitespace-normalised text
similarity (difflib ratio over the whole document), element counts per type,
TOC entry count and metadata. Exits 1 when similarity is below --min.
"""
import argparse
import collections
import difflib
import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
PRISM_ROOT = os.environ.get("PRISM_ROOT", "C:/Users/khais/Documents/GitHub/Prism-Document-Reader")
PRISM_PY = os.path.join(PRISM_ROOT, ".venv", "Scripts", "python.exe")


def run(cmd):
    env = dict(os.environ, PYTHONUTF8="1", PRISM_ROOT=PRISM_ROOT)
    r = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", env=env)
    if r.returncode != 0:
        sys.exit(f"{cmd[0]} failed:\n{r.stderr[-2000:]}")
    return json.loads(r.stdout)


def norm(s):
    return re.sub(r"\s+", " ", s).strip()


def count_toc(entries):
    return sum(1 + count_toc(e["children"]) for e in entries)


def elem_types(d):
    c = collections.Counter()
    for p in d["pages"]:
        for e in p["elements"]:
            c[re.sub(r"[_\s]", "", e["type"]).lower()] += 1
    return dict(sorted(c.items()))


ap = argparse.ArgumentParser()
ap.add_argument("file")
ap.add_argument("--min", type=float, default=0.90)
a = ap.parse_args()

prism = run([PRISM_PY, os.path.join(HERE, "prism_dump.py"), a.file])
cargo = ["cargo", "run", "-q", "--bin", "intome-dump", "--manifest-path", os.path.join(ROOT, "Cargo.toml"), "--", a.file]
intome = run(cargo)

pt = norm(" ".join(p["text"] for p in prism["pages"]))
it = norm(" ".join(p["text"] for p in intome["pages"]))
ratio = difflib.SequenceMatcher(None, pt, it, autojunk=False).ratio() if (pt or it) else 1.0

print(f"file:        {a.file}")
print(f"pages:       prism={len(prism['pages'])} intome={len(intome['pages'])}")
print(f"text chars:  prism={len(pt)} intome={len(it)}")
print(f"similarity:  {ratio:.3f} (min {a.min})")
print(f"elements:    prism={elem_types(prism)}")
print(f"             intome={elem_types(intome)}")
print(f"toc entries: prism={count_toc(prism['toc'])} intome={count_toc(intome['toc'])}")
print(f"metadata:    prism={prism['metadata']}")
print(f"             intome={intome['metadata']}")
if ratio < a.min:
    sm = difflib.SequenceMatcher(None, pt, it, autojunk=False)
    for tag, i1, i2, j1, j2 in [op for op in sm.get_opcodes() if op[0] != "equal"][:8]:
        print(f"  {tag}: prism[{pt[i1:i2][:120]!r}] intome[{it[j1:j2][:120]!r}]")
    sys.exit(1)
