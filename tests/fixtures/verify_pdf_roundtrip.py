r"""Check InTome's saved forms with PyMuPDF (an independent reader).

  cargo test --lib save_form_round_trips
  ..\Prism-Document-Reader\.venv\Scripts\python.exe -I tests\fixtures\verify_pdf_roundtrip.py

The Rust test writes its outputs to target/pdf-roundtrip.
"""
import os
import sys

import fitz

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "pdf-roundtrip")
MARKER = "intome-flatform-filled"

doc = fitz.open(os.path.join(OUT, "acroform_filled.pdf"))
expected = {
    # field name: (widget values, /TU)
    "name": (["Jane Doe"], "Your full name"),
    "email": (["jane@example.com"], "Email address"),
    "addr.city": (["Kuala Lumpur"], "City of residence"),
    "agree": (["Yes"], "I agree to the terms"),
    # One entry per radio widget: only the chosen button is on.
    "colour": (["Off", "Green", "Off"], "Favourite colour"),
    "country": (["Japan"], None),
    "fruit": (["ban"], "Pick a fruit"),
}
seen = {}
for w in doc[0].widgets():
    value = w.field_value
    if w.field_type == fitz.PDF_WIDGET_TYPE_RADIOBUTTON:
        value = w.on_state() if value not in (False, "Off") else "Off"
    seen.setdefault(w.field_name, ([], w.field_label))[0].append(value)
for name, want in expected.items():
    got = (seen[name][0], seen[name][1])
    assert got == want, f"{name}: got {got}, want {want}"
    print(f"ok  {name:10} values={got[0]} TU={got[1]!r}")
print("acroform_filled.pdf: NeedAppearances =", doc.xref_get_key(doc.pdf_catalog(), "AcroForm/NeedAppearances"))

flat = fitz.open(os.path.join(OUT, "flat_form_filled.pdf"))
text = flat[0].get_text()
assert "Ada Lovelace" in text, text
assert "X" in text.split(), text
assert MARKER in flat.metadata["keywords"], flat.metadata
print(f"ok  flat_form_filled.pdf: stamp text found, X mark found, keywords={flat.metadata['keywords']!r}")

enc = fitz.open(os.path.join(OUT, "password_stamped.pdf"))
assert enc.needs_pass, "encryption was dropped"
assert not enc.authenticate("wrong")
assert enc.authenticate("secret")
assert "Stamped note" in enc[0].get_text()
print(f"ok  password_stamped.pdf: still encrypted ({enc.metadata['encryption']}), stamp found after authenticating")
sys.exit(0)
