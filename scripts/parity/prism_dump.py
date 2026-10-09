"""Dump what Prism's original Python handler extracts from a document, as JSON.

Run with Prism's venv:
  ..\Prism-Document-Reader\.venv\Scripts\python.exe scripts\parity\prism_dump.py <file>
"""
import importlib
import json
import os
import pkgutil
import sys

PRISM_ROOT = os.path.join(os.path.dirname(__file__), "..", "..", "..", "Prism-Document-Reader")
sys.path.insert(0, os.path.abspath(PRISM_ROOT))
os.environ.setdefault("APPDATA", os.environ.get("TEMP", "."))

import prism.document.formats as formats  # noqa: E402
from prism.document.registry import registry  # noqa: E402

for m in pkgutil.iter_modules(formats.__path__):
    try:
        importlib.import_module(f"prism.document.formats.{m.name}")
    except Exception as exc:  # a missing optional lib just drops that format
        print(f"skip {m.name}: {exc}", file=sys.stderr)


def toc(entries):
    return [{"title": e.title, "page_index": e.page_index, "level": e.level, "children": toc(e.children)} for e in entries]


doc = registry.open_document(sys.argv[1])
pages = []
for i in range(doc.get_page_count()):
    p = doc.get_page(i)
    pages.append({
        "text": p.text,
        "elements": [
            {"type": e.element_type.name, "start": e.start_pos, "end": e.end_pos, "text": e.text}
            for e in p.semantic_elements
        ],
    })
md = doc.metadata
print(json.dumps({
    "metadata": {"title": md.get("title", ""), "author": md.get("author", ""), "format": md.get("format", "")},
    "toc": toc(doc.toc),
    "pages": pages,
}, ensure_ascii=False, indent=1))
