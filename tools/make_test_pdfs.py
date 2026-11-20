#!/usr/bin/env python3
"""Generates sample PDFs that exercise the converter's layout features.

Usage: python3 tools/make_test_pdfs.py OUTPUT_DIR

Requires `reportlab` (pip install reportlab). If `pdftoppm` (poppler) is
available, an image-only "scanned" PDF is produced as well for OCR testing.
"""
import os
import shutil
import subprocess
import sys

from reportlab.graphics.charts.barcharts import VerticalBarChart
from reportlab.graphics.shapes import Drawing
from reportlab.lib import colors
from reportlab.lib.enums import TA_CENTER, TA_JUSTIFY
from reportlab.lib.pagesizes import A4, letter
from reportlab.lib.styles import ParagraphStyle, getSampleStyleSheet
from reportlab.lib.units import inch
from reportlab.pdfgen import canvas
from reportlab.platypus import (BaseDocTemplate, Frame, Image, ListFlowable, ListItem, PageTemplate,
                                Paragraph, SimpleDocTemplate, Spacer, Table, TableStyle)

LOREM = (
    "Layout preservation means that paragraphs keep their alignment, indentation and spacing, "
    "that fonts keep their size and weight, and that tables, lists and images stay where the "
    "author put them. This paragraph is long enough to wrap over several lines so that the "
    "converter has to recognise it as a single justified paragraph rather than as separate lines."
)


def make_logo(path):
    from PIL import Image as PILImage, ImageDraw
    img = PILImage.new("RGB", (240, 120), (31, 78, 121))
    d = ImageDraw.Draw(img)
    d.ellipse((24, 24, 96, 96), fill=(244, 177, 131))
    d.rectangle((120, 50, 216, 70), fill=(255, 255, 255))
    img.save(path)


def chart():
    d = Drawing(300, 160)
    bc = VerticalBarChart()
    bc.x, bc.y, bc.height, bc.width = 40, 30, 110, 240
    bc.data = [(13, 25, 31, 22, 40), (9, 18, 27, 30, 35)]
    bc.categoryAxis.categoryNames = ["Q1", "Q2", "Q3", "Q4", "Q5"]
    bc.bars[0].fillColor = colors.HexColor("#4472c4")
    bc.bars[1].fillColor = colors.HexColor("#ed7d31")
    bc.valueAxis.valueMin = 0
    d.add(bc)
    return d


def report(path, logo):
    ss = getSampleStyleSheet()
    body = ParagraphStyle("body", parent=ss["BodyText"], fontName="Times-Roman", fontSize=11, leading=14,
                          alignment=TA_JUSTIFY, spaceAfter=8)
    indented = ParagraphStyle("ind", parent=body, firstLineIndent=24, spaceAfter=0)
    title = ParagraphStyle("title", parent=ss["Title"], fontName="Helvetica-Bold", fontSize=24, leading=28,
                           alignment=TA_CENTER, textColor=colors.HexColor("#1f4e79"))
    h1 = ParagraphStyle("h1", parent=ss["Heading1"], fontName="Helvetica-Bold", fontSize=16, leading=20,
                        spaceBefore=10, spaceAfter=6)
    h2 = ParagraphStyle("h2", parent=ss["Heading2"], fontName="Helvetica-Bold", fontSize=13, leading=16,
                        spaceBefore=8, spaceAfter=4, textColor=colors.HexColor("#c00000"))

    def footer(c, doc):
        c.saveState()
        c.setFont("Helvetica", 9)
        c.drawString(72, 40, "Quarterly Report - Confidential")
        c.drawRightString(A4[0] - 72, 40, f"Page {doc.page}")
        c.setStrokeColor(colors.grey)
        c.line(72, 52, A4[0] - 72, 52)
        c.restoreState()

    doc = SimpleDocTemplate(path, pagesize=A4, leftMargin=72, rightMargin=72, topMargin=72, bottomMargin=72,
                            title="Quarterly Report", author="pdf2word tests")
    story = [
        Image(logo, width=120, height=60),
        Paragraph("Quarterly Business Report", title),
        Paragraph("<i>Prepared for the board of directors</i>", ParagraphStyle("sub", parent=body, alignment=TA_CENTER)),
        Spacer(1, 12),
        Paragraph("1. Executive Summary", h1),
        Paragraph(LOREM, body),
        Paragraph("Text can be <b>bold</b>, <i>italic</i>, <font color='#c00000'>red</font>, "
                  "<u>underlined</u>, or contain E = mc<super>2</super> and H<sub>2</sub>O. Visit "
                  "<a href='https://example.com/report' color='blue'>our website</a> for details.", body),
        Paragraph("1.1 Key Points", h2),
        ListFlowable([
            ListItem(Paragraph("Revenue grew by 12% compared to the previous quarter, driven by new contracts.", body)),
            ListItem(Paragraph("Operating costs decreased thanks to the consolidation of two data centres.", body)),
            ListItem(Paragraph("Customer satisfaction reached an all-time high.", body)),
        ], bulletType="bullet", start="•", leftIndent=18),
        Paragraph("2. Financial Results", h1),
        Paragraph(LOREM, indented),
        Paragraph("A second paragraph with a first-line indent follows directly without extra spacing. "
                  "It continues for a while so that it wraps onto a second and third line in the output.", indented),
        Spacer(1, 10),
    ]
    data = [
        ["Region", "Q1", "Q2", "Q3", "Total"],
        ["North", "1,200", "1,350", "1,410", "3,960"],
        ["South", "980", "1,020", "1,100", "3,100"],
        ["East", "Not reported", "", "760", "760"],
        ["Total", "2,180", "2,370", "3,270", "7,820"],
    ]
    t = Table(data, colWidths=[110, 70, 70, 70, 80])
    t.setStyle(TableStyle([
        ("GRID", (0, 0), (-1, -1), 0.75, colors.black),
        ("BACKGROUND", (0, 0), (-1, 0), colors.HexColor("#d9e2f3")),
        ("FONTNAME", (0, 0), (-1, 0), "Helvetica-Bold"),
        ("FONTNAME", (0, 1), (-1, -1), "Helvetica"),
        ("FONTSIZE", (0, 0), (-1, -1), 10),
        ("ALIGN", (1, 0), (-1, -1), "RIGHT"),
        ("SPAN", (1, 3), (2, 3)),
        ("FONTNAME", (0, -1), (-1, -1), "Helvetica-Bold"),
    ]))
    story += [t, Spacer(1, 14), Paragraph("Figure 1: Revenue by quarter", ParagraphStyle("cap", parent=body, alignment=TA_CENTER, fontSize=9)), chart()]
    story += [Paragraph("3. Outlook", h1)] + [Paragraph(LOREM, body) for _ in range(3)]
    doc.build(story, onFirstPage=footer, onLaterPages=footer)


def two_columns(path):
    ss = getSampleStyleSheet()
    body = ParagraphStyle("b", parent=ss["BodyText"], fontName="Times-Roman", fontSize=10, leading=12.5, alignment=TA_JUSTIFY, spaceAfter=6)
    head = ParagraphStyle("h", parent=ss["Title"], fontName="Times-Bold", fontSize=20, leading=24)
    doc = BaseDocTemplate(path, pagesize=letter, leftMargin=54, rightMargin=54, topMargin=54, bottomMargin=54)
    w, h = letter
    gutter = 18
    colw = (w - 108 - gutter) / 2
    title_frame = Frame(54, h - 54 - 60, w - 108, 60, id="title")
    left = Frame(54, 54, colw, h - 108 - 64, id="left")
    right = Frame(54 + colw + gutter, 54, colw, h - 108 - 64, id="right")
    doc.addPageTemplates([PageTemplate(id="two", frames=[title_frame, left, right])])
    story = [Paragraph("A Study of Two-Column Layouts", head)]
    for i in range(1, 15):
        story.append(Paragraph(f"<b>Section {i}.</b> " + LOREM, body))
    doc.build(story)


def invoice(path):
    c = canvas.Canvas(path, pagesize=letter)
    w, h = letter
    c.setFont("Helvetica-Bold", 22)
    c.drawString(72, h - 90, "INVOICE")
    c.setFont("Helvetica", 10)
    c.drawRightString(w - 72, h - 80, "Invoice #: 2024-0042")
    c.drawRightString(w - 72, h - 94, "Date: 18 November 2024")
    y = h - 140
    for line in ["Bill to:", "Jane Doe", "42 Example Street", "Springfield, 12345"]:
        c.drawString(72, y, line)
        y -= 14
    y -= 20
    c.setFont("Helvetica-Bold", 10)
    cols = [72, 300, 380, 470]
    for x, t in zip(cols, ["Description", "Qty", "Unit price", "Amount"]):
        c.drawString(x, y, t)
    c.line(72, y - 4, w - 72, y - 4)
    c.setFont("Helvetica", 10)
    rows = [("Consulting services", "10", "$120.00", "$1,200.00"),
            ("Software licence (annual)", "1", "$499.00", "$499.00"),
            ("Training workshop", "2", "$350.00", "$700.00"),
            ("Travel expenses", "1", "$180.50", "$180.50")]
    y -= 20
    for r in rows:
        for x, t in zip(cols, r):
            c.drawString(x, y, t)
        y -= 16
    c.line(72, y + 8, w - 72, y + 8)
    c.setFont("Helvetica-Bold", 11)
    c.drawString(380, y - 8, "Total")
    c.drawString(470, y - 8, "$2,579.50")
    c.setFont("Helvetica-Oblique", 9)
    c.drawCentredString(w / 2, 60, "Thank you for your business!")
    c.showPage()
    c.save()


def font_widths(path):
    """Text whose widths differ from Word's fonts (condensed, letter-spaced)."""
    c = canvas.Canvas(path, pagesize=letter)
    w, h = letter
    text = (LOREM + " ") * 2
    y = h - 72

    def para(font, size, leading, scale=100, charspace=0.0, width=w - 144):
        nonlocal y
        words = text.split()
        line = []
        for word in words + [None]:
            cand = " ".join(line + ([word] if word else []))
            tw = c.stringWidth(cand, font, size) * scale / 100 + charspace * len(cand)
            if word is None or (tw > width and line):
                t = c.beginText(72, y)
                t.setFont(font, size)
                t.setHorizScale(scale)
                t.setCharSpace(charspace)
                t.textOut(" ".join(line))
                c.drawText(t)
                y -= leading
                line = [word] if word else []
            else:
                line.append(word)
        y -= leading

    c.setFont("Helvetica-Bold", 13)
    c.drawString(72, y, "Condensed Helvetica (82% width)")
    y -= 20
    para("Helvetica", 10, 13, scale=82)
    c.setFont("Helvetica-Bold", 13)
    c.drawString(72, y, "Letter-spaced Times (+0.6 pt)")
    y -= 20
    para("Times-Roman", 10.5, 13.5, charspace=0.6)
    c.setFont("Helvetica-Bold", 13)
    c.drawString(72, y, "Regular Courier (control)")
    y -= 20
    para("Courier", 9, 12)
    c.showPage()
    c.save()


def embedded_font(path, workdir):
    """Text in a TrueType font that no system has ("Fixture Sans", a renamed Carlito)."""
    try:
        from fontTools.ttLib import TTFont as FTFont
    except ImportError:
        print("fontTools not installed; skipping embedded.pdf")
        return
    src = "/usr/share/fonts/truetype/crosextra/Carlito-Regular.ttf"
    if not os.path.exists(src):
        print("Carlito not installed; skipping embedded.pdf")
        return
    f = FTFont(src)
    for rec in f["name"].names:
        if rec.nameID in (1, 3, 4, 16):
            rec.string = "Fixture Sans"
        elif rec.nameID == 6:
            rec.string = "FixtureSans"
    ttf = os.path.join(workdir, "FixtureSans.ttf")
    f.save(ttf)
    from reportlab.pdfbase import pdfmetrics
    from reportlab.pdfbase.ttfonts import TTFont as RLTTFont
    pdfmetrics.registerFont(RLTTFont("FixtureSans", ttf))
    ss = getSampleStyleSheet()
    body = ParagraphStyle("fx", parent=ss["BodyText"], fontName="FixtureSans", fontSize=12, leading=15, alignment=TA_JUSTIFY)
    head = ParagraphStyle("fxh", parent=body, fontSize=20, leading=24, alignment=TA_CENTER)
    doc = SimpleDocTemplate(path, pagesize=letter)
    doc.build([Paragraph("Embedded Fixture Sans", head), Spacer(1, 12)] + [Paragraph(LOREM, body) for _ in range(3)])


def typography(path):
    """One line per font style, for OCR font/bold/italic detection."""
    c = canvas.Canvas(path, pagesize=letter)
    w, h = letter
    y = h - 90
    styles = [
        ("Times-Roman", "Times regular text for the classifier"),
        ("Times-Bold", "Times bold text for the classifier"),
        ("Times-Italic", "Times italic text for the classifier"),
        ("Helvetica", "Helvetica regular text for the classifier"),
        ("Helvetica-Bold", "Helvetica bold text for the classifier"),
        ("Helvetica-Oblique", "Helvetica oblique text for the classifier"),
        ("Courier", "Courier monospaced text for the classifier"),
        ("Courier-Bold", "Courier bold text for the classifier"),
    ]
    for font, text in styles:
        c.setFont(font, 14)
        c.drawString(72, y, text)
        y -= 40
    c.showPage()
    c.save()


def rtl(path):
    """Hebrew and Arabic paragraphs drawn in visual order (as many generators do)."""
    try:
        from bidi.algorithm import get_display
        import arabic_reshaper
    except ImportError:
        print("python-bidi / arabic-reshaper not installed; skipping rtl.pdf")
        return
    from reportlab.pdfbase import pdfmetrics
    from reportlab.pdfbase.ttfonts import TTFont as RLTTFont
    font_path = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
    if not os.path.exists(font_path):
        print("DejaVu Sans not installed; skipping rtl.pdf")
        return
    pdfmetrics.registerFont(RLTTFont("DejaVuSans", font_path))
    c = canvas.Canvas(path, pagesize=A4)
    w, h = A4
    right, width = w - 72, w - 144
    y = h - 90

    def lines_of(text, size):
        out, line = [], []
        for word in text.split():
            cand = " ".join(line + [word])
            if c.stringWidth(get_display(arabic_reshaper.reshape(cand)), "DejaVuSans", size) > width and line:
                out.append(" ".join(line))
                line = [word]
            else:
                line.append(word)
        out.append(" ".join(line))
        return out

    def para(text, size=12, leading=17, align="right"):
        nonlocal y
        for ln in lines_of(text, size):
            vis = get_display(arabic_reshaper.reshape(ln))
            c.setFont("DejaVuSans", size)
            if align == "center":
                c.drawCentredString(w / 2, y, vis)
            else:
                c.drawRightString(right, y, vis)
            y -= leading
        y -= 10

    para("מסמך לדוגמה בעברית", 20, 26, "center")
    para("זוהי פסקה ארוכה בעברית שנועדה לבדוק את ההמרה של טקסט מימין לשמאל. היא כוללת את המספר 2024 "
         "ואת המילה English באמצע המשפט, וממשיכה מספיק זמן כדי לעבור לשורה הבאה ולבדוק את סדר המילים.")
    para("نص عربي تجريبي", 20, 26, "center")
    para("هذه فقرة طويلة باللغة العربية لاختبار تحويل النص من اليمين إلى اليسار. تحتوي على الرقم 2024 "
         "وكلمة English في منتصف الجملة، وتستمر لفترة كافية للانتقال إلى السطر التالي.")
    c.showPage()
    c.save()


def rtl_office(path, workdir):
    """The same kind of content exported by LibreOffice (real shaping and bidi)."""
    if not shutil.which("soffice"):
        print("LibreOffice not installed; skipping rtl-office.pdf")
        return
    html = os.path.join(workdir, "rtl-office.html")
    with open(html, "w", encoding="utf-8") as f:
        f.write("""<html><head><meta charset="utf-8"></head><body style="font-family:'DejaVu Sans'">
<h1 dir="rtl" style="text-align:center">מסמך לדוגמה בעברית</h1>
<p dir="rtl" style="text-align:justify">זוהי פסקה ארוכה בעברית שנועדה לבדוק את ההמרה של טקסט מימין לשמאל. היא כוללת את המספר 2024
ואת המילה English באמצע המשפט (בסוגריים), וממשיכה מספיק זמן כדי לעבור לשורה הבאה ולבדוק את סדר המילים.</p>
<p dir="rtl">هذه فقرة طويلة باللغة العربية لاختبار تحويل النص من اليمين إلى اليسار. تحتوي على الرقم 2024
وكلمة English في منتصف الجملة، وتستمر لفترة كافية للانتقال إلى السطر التالي.</p>
<p>An English paragraph with a Hebrew word שלום in the middle stays left to right.</p>
</body></html>""")
    subprocess.run(["soffice", "--headless", "--convert-to", "pdf:writer_web_pdf_Export", "--outdir", workdir, html],
                   check=True, capture_output=True)
    os.replace(os.path.join(workdir, "rtl-office.pdf"), path)


def rotated(path):
    """Rotated sidebar, angled watermark and vertical Japanese text."""
    c = canvas.Canvas(path, pagesize=letter)
    w, h = letter
    c.setFont("Helvetica-Bold", 18)
    c.drawString(72, h - 80, "Rotated and vertical text")
    c.setFont("Times-Roman", 11)
    t = c.beginText(72, h - 110)
    for line in ["Body text stays horizontal. The sidebar on the left edge is rotated by ninety",
                 "degrees, the watermark is drawn at thirty degrees and the column on the right",
                 "uses vertical Japanese writing."]:
        t.textLine(line)
    c.drawText(t)
    # Sidebar reading bottom to top.
    c.saveState()
    c.translate(40, 300)
    c.rotate(90)
    c.setFont("Helvetica", 12)
    c.setFillColor(colors.HexColor("#1f4e79"))
    c.drawString(0, 0, "Confidential sidebar label")
    c.restoreState()
    # Watermark.
    c.saveState()
    c.translate(300, 400)
    c.rotate(30)
    c.setFont("Helvetica-Bold", 60)
    c.setFillColor(colors.HexColor("#d0d0d0"))
    c.drawCentredString(0, 0, "DRAFT")
    c.restoreState()
    # Vertical Japanese (one character per row, top to bottom).
    try:
        from reportlab.pdfbase import pdfmetrics
        from reportlab.pdfbase.cidfonts import UnicodeCIDFont
        pdfmetrics.registerFont(UnicodeCIDFont("HeiseiMin-W3"))
        c.setFont("HeiseiMin-W3", 16)
        y = h - 160
        for ch in "縦書きの日本語":
            c.drawString(w - 90, y, ch)
            y -= 18
    except Exception as e:  # pragma: no cover
        print("CID font unavailable:", e)
    c.showPage()
    c.save()


def scanned(src_pdf, path, workdir):
    if not shutil.which("pdftoppm"):
        print("pdftoppm not found; skipping scanned.pdf")
        return
    stem = "scan-" + os.path.splitext(os.path.basename(src_pdf))[0]
    prefix = os.path.join(workdir, stem)
    subprocess.run(["pdftoppm", "-r", "200", "-f", "1", "-l", "1", "-png", src_pdf, prefix], check=True)
    png = [f for f in os.listdir(workdir) if f.startswith(stem) and f.endswith(".png")][0]
    c = canvas.Canvas(path, pagesize=A4)
    c.drawImage(os.path.join(workdir, png), 0, 0, width=A4[0], height=A4[1])
    c.showPage()
    c.save()


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "."
    os.makedirs(out, exist_ok=True)
    logo = os.path.join(out, "logo.png")
    make_logo(logo)
    report(os.path.join(out, "report.pdf"), logo)
    two_columns(os.path.join(out, "twocol.pdf"))
    invoice(os.path.join(out, "invoice.pdf"))
    font_widths(os.path.join(out, "fonts.pdf"))
    embedded_font(os.path.join(out, "embedded.pdf"), out)
    scanned(os.path.join(out, "report.pdf"), os.path.join(out, "scanned.pdf"), out)
    typography(os.path.join(out, "typography.pdf"))
    rtl(os.path.join(out, "rtl.pdf"))
    rotated(os.path.join(out, "rotated.pdf"))
    rtl_office(os.path.join(out, "rtl-office.pdf"), out)
    scanned(os.path.join(out, "typography.pdf"), os.path.join(out, "typography-scan.pdf"), out)
    print("written to", out)


if __name__ == "__main__":
    main()
