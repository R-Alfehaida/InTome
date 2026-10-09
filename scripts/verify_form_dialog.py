r"""Checks PDFs saved through the Fill Form dialog (examples/form_demo.rs) with PyMuPDF.

  ..\Prism-Document-Reader\.venv\Scripts\python.exe -I scripts\verify_form_dialog.py acroform <pdf>
  ..\Prism-Document-Reader\.venv\Scripts\python.exe -I scripts\verify_form_dialog.py flat <pdf>
"""
import sys

import fitz

kind, path = sys.argv[1], sys.argv[2]
doc = fitz.open(path)
if kind == "acroform":
    expected = {
        "name": (["Jane Doe"], "Your full name"),
        "email": (["jane@example.com"], "Email address"),
        "addr.city": (["Kuala Lumpur"], "City of residence"),
        "agree": (["Yes"], "I agree to the terms"),
        "colour": (["Off", "Green", "Off"], "Favourite colour"),
        "country": (["Japan"], "Country & region"),
        "fruit": (["ban"], "Pick a fruit"),
    }
    seen = {}
    for w in doc[0].widgets():
        value = w.field_value
        if w.field_type == fitz.PDF_WIDGET_TYPE_RADIOBUTTON:
            value = w.on_state() if value not in (False, "Off") else "Off"
        seen.setdefault(w.field_name, ([], w.field_label))[0].append(value)
    for name, want in expected.items():
        got = seen[name]
        assert got == want, f"{name}: got {got}, want {want}"
        print(f"ok  {name:10} values={got[0]} TU={got[1]!r}")
else:
    text = doc[0].get_text()
    words = text.split()
    assert "Ada Lovelace" in text, text
    assert "1815-12-10" in text, text
    assert words.count("X") == 2, words
    assert "intome-flatform-filled" in doc.metadata["keywords"], doc.metadata
    print(f"ok  stamped name and date, {words.count('X')} X marks, keywords={doc.metadata['keywords']!r}")
