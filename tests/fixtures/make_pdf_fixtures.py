r"""Regenerate the PDF fixtures used by src/document/formats/pdf.rs tests and parity runs.

  ..\Prism-Document-Reader\.venv\Scripts\python.exe -I tests\fixtures\make_pdf_fixtures.py

Needs PyMuPDF and fpdf2 (both in Prism's venv). Writes next to this script.
"""
import os

import fitz
from fpdf import FPDF

OUT = os.path.dirname(os.path.abspath(__file__))
LOREM = (
    "Accessible reading starts with text a screen reader can reach. "
    "This paragraph exists so every page carries enough extractable words to count as searchable. "
)


def path(name):
    return os.path.join(OUT, name)


def text_pdf():
    pdf = FPDF()
    pdf.set_title("Fixture Handbook")
    pdf.set_author("InTome Tests")
    pdf.set_font("Helvetica", size=12)
    outline = [
        (0, "Part One"), (1, "Chapter 1"), (2, "Section 1.1"), (1, "Chapter 2"),
        (0, "Part Two"), (1, "Chapter 3"),
    ]
    for level, title in outline:
        pdf.add_page()
        pdf.start_section(title, level=level)
        pdf.cell(0, 10, title, new_x="LMARGIN", new_y="NEXT")
        pdf.multi_cell(0, 6, (LOREM + f"Page about {title}. ") * 4)
    pdf.output(path("text_toc.pdf"))


def two_column_pdf():
    pdf = FPDF()
    pdf.set_font("Helvetica", size=11)
    pdf.add_page()
    pdf.cell(0, 10, "Two Column Newsletter", new_x="LMARGIN", new_y="NEXT")
    top = pdf.get_y()
    left = "Left column. " + "The left column talks about apples and orchards. " * 8
    right = "Right column. " + "The right column talks about rivers and bridges. " * 8
    pdf.set_xy(10, top)
    pdf.multi_cell(90, 6, left)
    pdf.set_xy(110, top)
    pdf.multi_cell(90, 6, right)
    pdf.output(path("two_column.pdf"))


def password_pdf():
    doc = fitz.open(path("text_toc.pdf"))
    doc.save(
        path("password.pdf"),
        encryption=fitz.PDF_ENCRYPT_AES_256,
        user_pw="secret",
        owner_pw="owner-secret",
    )


def scanned_pdf():
    src = fitz.open()
    page = src.new_page(width=612, height=792)
    page.insert_text((72, 100), "This page was scanned and has no text layer.", fontsize=18)
    pix = page.get_pixmap(dpi=100)
    out = fitz.open()
    p = out.new_page(width=612, height=792)
    p.insert_image(p.rect, pixmap=pix)
    out.save(path("scanned.pdf"))


def flat_form_pdf():
    pdf = FPDF()
    pdf.set_font("Helvetica", size=12)
    pdf.add_page()
    pdf.cell(0, 10, "Membership Application", new_x="LMARGIN", new_y="NEXT")
    pdf.cell(0, 10, "Full name: ______________________________", new_x="LMARGIN", new_y="NEXT")
    pdf.cell(0, 10, "Date of birth: ____________________", new_x="LMARGIN", new_y="NEXT")
    pdf.cell(0, 10, "Signature:", new_x="LMARGIN", new_y="NEXT")
    pdf.line(40, pdf.get_y() - 2, 140, pdf.get_y() - 2)
    pdf.output(path("flat_form.pdf"))


def raw_pdf(objects, out_name):
    """objects: list of bytes bodies; object n is objects[n-1]. Object 1 is the catalog."""
    buf = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = []
    for i, body in enumerate(objects, start=1):
        offsets.append(len(buf))
        buf += f"{i} 0 obj\n".encode() + body + b"\nendobj\n"
    xref = len(buf)
    buf += f"xref\n0 {len(objects) + 1}\n0000000000 65535 f \n".encode()
    for off in offsets:
        buf += f"{off:010d} 00000 n \n".encode()
    buf += f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    with open(path(out_name), "wb") as f:
        f.write(buf)


def stream(dict_body, data):
    return f"<< {dict_body} /Length {len(data)} >>\nstream\n".encode() + data + b"\nendstream"


def acroform_pdf():
    # Object numbers are fixed so the bodies can cross-reference each other.
    labels = (
        b"BT /Helv 11 Tf 72 712 Td (Full name) Tj 0 -40 Td (Email) Tj 0 -40 Td (City) Tj "
        b"0 -40 Td (I agree) Tj 0 -40 Td (Colour: Red Green Blue) Tj 0 -40 Td (Country) Tj "
        b"0 -60 Td (Fruit) Tj ET"
    )
    tx_da = "/DA (/Helv 11 Tf 0 g)"
    widget = "/Type /Annot /Subtype /Widget /F 4 /P 3 0 R"
    on_ap = stream("/Type /XObject /Subtype /Form /BBox [0 0 14 14]", b"0 g 3 3 8 8 re f")
    off_ap = stream("/Type /XObject /Subtype /Form /BBox [0 0 14 14]", b"")
    objs = {
        1: b"<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R "
           b"/Resources << /Font << /Helv 6 0 R >> >> "
           b"/Annots [7 0 R 8 0 R 10 0 R 11 0 R 13 0 R 14 0 R 15 0 R 16 0 R 17 0 R] >>",
        4: b"<< /Fields [7 0 R 8 0 R 9 0 R 11 0 R 12 0 R 16 0 R 17 0 R] "
           b"/DR << /Font << /Helv 6 0 R >> >> /DA (/Helv 0 Tf 0 g) >>",
        5: stream("", labels),
        6: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        7: f"<< {widget} /FT /Tx /T (name) /TU (Your full name) {tx_da} /Rect [180 705 400 725] >>".encode(),
        8: f"<< {widget} /FT /Tx /T (email) /V (old@example.com) {tx_da} /Rect [180 665 400 685] >>".encode(),
        9: b"<< /T (addr) /Kids [10 0 R] >>",
        10: f"<< {widget} /Parent 9 0 R /FT /Tx /T (city) /TU (City of residence) {tx_da} "
            f"/Rect [180 625 400 645] >>".encode(),
        11: f"<< {widget} /FT /Btn /T (agree) /TU (I agree to the terms) /V /Off /AS /Off "
            f"/Rect [180 585 194 599] /AP << /N << /Yes 18 0 R /Off 19 0 R >> >> >>".encode(),
        12: b"<< /FT /Btn /Ff 49152 /T (colour) /TU (Favourite colour) /V /Off /Kids [13 0 R 14 0 R 15 0 R] >>",
        13: f"<< {widget} /Parent 12 0 R /AS /Off /Rect [250 545 264 559] "
            f"/AP << /N << /Red 18 0 R /Off 19 0 R >> >> >>".encode(),
        14: f"<< {widget} /Parent 12 0 R /AS /Off /Rect [300 545 314 559] "
            f"/AP << /N << /Green 18 0 R /Off 19 0 R >> >> >>".encode(),
        15: f"<< {widget} /Parent 12 0 R /AS /Off /Rect [350 545 364 559] "
            f"/AP << /N << /Blue 18 0 R /Off 19 0 R >> >> >>".encode(),
        16: f"<< {widget} /FT /Ch /Ff 131072 /T (country) /Opt [(Malaysia) (Singapore) (Japan)] "
            f"/V (Malaysia) {tx_da} /Rect [180 505 400 525] >>".encode(),
        17: f"<< {widget} /FT /Ch /T (fruit) /TU (Pick a fruit) "
            f"/Opt [[(app) (Apple)] [(ban) (Banana)] [(che) (Cherry)]] {tx_da} /Rect [180 420 400 470] >>".encode(),
        18: on_ap,
        19: off_ap,
    }
    raw_pdf([objs[i] for i in range(1, len(objs) + 1)], "acroform.pdf")


if __name__ == "__main__":
    text_pdf()
    two_column_pdf()
    password_pdf()
    scanned_pdf()
    flat_form_pdf()
    acroform_pdf()
    for name in sorted(os.listdir(OUT)):
        if name.endswith(".pdf"):
            d = fitz.open(path(name)) if name != "password.pdf" else None
            info = f"{d.page_count} pages, toc {len(d.get_toc())}, widgets {sum(len(list(p.widgets())) for p in d)}" if d else "encrypted"
            print(f"{name}: {info}")
