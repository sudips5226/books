#!/usr/bin/env python3
"""Conversion-quality harness.

    python3 tools/eval.py fetch                 # download eval/corpus.json into eval/corpus
    python3 tools/eval.py run --bin target/release/pdf2word [--modes flow,exact] [--only NAME]
    python3 tools/eval.py compare eval/report/results.json eval/baseline.json

For every document the PDF is converted, the DOCX is rendered back to PDF with
LibreOffice, and both are compared:

* visual  – symmetric ink agreement of the rendered pages (blurred masks, 0..1)
* text_f1 – bag-of-words F1 between the PDF text and the text of the rendered DOCX
* order   – similarity of the word sequences (reading order)
* pages   – page-count agreement (min/max)
* lines   – share of PDF lines whose break is reproduced (flow mode only)

Requires: LibreOffice (with its Math component), poppler-utils, Pillow.
LibreOffice stands in for Word as the renderer, so absolute numbers include
its own differences (kerning, font substitution); use them to compare
versions of the converter, not as an absolute truth.
"""
import argparse
import difflib
import hashlib
import html
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unicodedata
import urllib.request
import zipfile
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EVAL = ROOT / "eval"
FIXTURES = ROOT / "crates" / "core" / "tests" / "fixtures"


def load_corpus():
    docs = json.loads((EVAL / "corpus.json").read_text())
    # The repository's own fixtures are part of the corpus as well.
    for p in sorted(FIXTURES.glob("*.pdf")):
        docs.append({"name": "fixture-" + p.stem, "path": str(p), "pages": None, "notes": "test fixture"})
    return docs


def doc_path(d):
    return Path(d["path"]) if "path" in d else EVAL / "corpus" / (d["name"] + ".pdf")


def cmd_fetch(args):
    (EVAL / "corpus").mkdir(parents=True, exist_ok=True)
    manifest = json.loads((EVAL / "corpus.json").read_text())
    changed = False
    for d in manifest:
        dest = doc_path(d)
        if not dest.exists():
            print("downloading", d["name"])
            with urllib.request.urlopen(d["url"], timeout=120) as r:
                dest.write_bytes(r.read())
        digest = hashlib.sha256(dest.read_bytes()).hexdigest()
        if "sha256" not in d:
            d["sha256"] = digest
            changed = True
        elif d["sha256"] != digest:
            sys.exit(f"{d['name']}: checksum mismatch ({digest})")
    if changed:
        (EVAL / "corpus.json").write_text(json.dumps(manifest, indent=1, ensure_ascii=False) + "\n")
    print(f"{len(manifest)} documents ready in {EVAL / 'corpus'}")


# ---------------------------------------------------------------- metrics


def words(text):
    text = unicodedata.normalize("NFKC", text).replace("­", "").lower()
    return re.findall(r"\w+", text)


def pdf_text(pdf, pages=None):
    cmd = ["pdftotext", "-q", "-cropbox"]
    if pages:
        a, _, b = pages.partition("-")
        cmd += ["-f", a, "-l", b or a]
    out = subprocess.run(cmd + [str(pdf), "-"], capture_output=True, text=True, errors="replace")
    return out.stdout


def docx_text(path):
    parts = []
    with zipfile.ZipFile(path) as z:
        names = [n for n in z.namelist() if re.match(r"word/(document|footnotes)\.xml$", n)]
        for n in sorted(names, key=lambda n: (n != "word/document.xml", n)):
            xml = z.read(n).decode("utf-8", "replace")
            xml = re.sub(r"</w:p>", "\n", xml)
            xml = re.sub(r"<w:tab/>", " ", xml)
            parts += re.findall(r"<(?:w|m):t(?: [^>]*)?>([^<]*)</(?:w|m):t>", xml)
            parts.append("\n")
    return html.unescape(" ".join(parts))


def text_scores(orig, conv):
    a, b = words(orig), words(conv)
    if not a and not b:
        return 1.0, 1.0
    ca, cb = Counter(a), Counter(b)
    common = sum((ca & cb).values())
    p = common / max(1, sum(cb.values()))
    r = common / max(1, sum(ca.values()))
    f1 = 0.0 if p + r == 0 else 2 * p * r / (p + r)
    order = difflib.SequenceMatcher(None, a[:3000], b[:3000], autojunk=False).ratio()
    return round(f1, 4), round(order, 4)


def render(pdf, outdir, prefix, pages=None, dpi=36, limit=10):
    cmd = ["pdftoppm", "-q", "-gray", "-r", str(dpi), "-png"]
    first = 1
    if pages:
        a, _, b = pages.partition("-")
        first = int(a)
        cmd += ["-f", a, "-l", b or a]
    else:
        cmd += ["-l", str(limit)]
    subprocess.run(cmd + [str(pdf), str(outdir / prefix)], capture_output=True)
    return sorted(outdir.glob(prefix + "-*.png"))[:limit]


def visual_score(orig_imgs, conv_imgs):
    from PIL import Image, ImageChops, ImageFilter

    scores = []
    for i, o in enumerate(orig_imgs):
        if i >= len(conv_imgs):
            scores.append(0.0)
            continue
        a = Image.open(o).convert("L")
        b = Image.open(conv_imgs[i]).convert("L").resize(a.size)
        ma = a.point(lambda v: 255 if v < 170 else 0)
        mb = b.point(lambda v: 255 if v < 170 else 0)
        da, db = ma.filter(ImageFilter.MaxFilter(5)), mb.filter(ImageFilter.MaxFilter(5))
        ink_a, ink_b = sum(1 for v in ma.getdata() if v), sum(1 for v in mb.getdata() if v)
        if ink_a + ink_b == 0:
            scores.append(1.0)
            continue
        hit_a = sum(1 for v in ImageChops.multiply(ma, db).getdata() if v)
        hit_b = sum(1 for v in ImageChops.multiply(mb, da).getdata() if v)
        scores.append((hit_a + hit_b) / (ink_a + ink_b))
    return round(sum(scores) / len(scores), 4) if scores else 0.0


def line_breaks(orig_pdf, conv_pdf, pages=None):
    def lines(pdf, pg):
        cmd = ["pdftotext", "-q", "-cropbox", "-layout"]
        if pg:
            a, _, b = pg.partition("-")
            cmd += ["-f", a, "-l", b or a]
        out = subprocess.run(cmd + [str(pdf), "-"], capture_output=True, text=True, errors="replace").stdout
        return [w for w in (re.findall(r"\w+", l.lower()) for l in out.splitlines()) if len(w) >= 3]

    a, b = lines(orig_pdf, pages), lines(conv_pdf, None)
    ends = Counter((l[-1], len(l)) for l in b)
    hit = 0
    for l in a:
        k = (l[-1], len(l))
        if ends[k] > 0:
            ends[k] -= 1
            hit += 1
    return round(hit / len(a), 4) if a else 1.0


def page_count(pdf, pages=None):
    if pages:
        a, _, b = pages.partition("-")
        return int(b or a) - int(a) + 1
    out = subprocess.run(["pdfinfo", str(pdf)], capture_output=True, text=True, errors="replace").stdout
    m = re.search(r"Pages:\s+(\d+)", out)
    return int(m.group(1)) if m else 0


# ---------------------------------------------------------------- run


def cmd_run(args):
    out = Path(args.out).resolve()
    if out.exists():
        shutil.rmtree(out)
    (out / "img").mkdir(parents=True)
    work = Path(tempfile.mkdtemp(prefix="pdf2word-eval-"))
    modes = args.modes.split(",")
    docs = [d for d in load_corpus() if not args.only or args.only in d["name"]]
    env = dict(os.environ)
    results = []
    for d in docs:
        src = doc_path(d)
        if not src.exists():
            print("missing", src, "- run `fetch` first")
            continue
        for mode in modes:
            name = f"{d['name']}.{mode}"
            docx = work / f"{name}.docx"
            cmd = [args.bin, "-q", "-m", mode, str(src), "-o", str(docx)] + (args.args.split() if args.args else [])
            if d.get("pages"):
                cmd += ["-p", d["pages"]]
            t0 = time.time()
            try:
                proc = subprocess.run(cmd, capture_output=True, text=True, timeout=600, env=env)
                ok = proc.returncode == 0 and docx.exists()
                err = proc.stderr.strip().splitlines()[-1:] if not ok else []
            except subprocess.TimeoutExpired:
                ok, err = False, ["timeout"]
            results.append({"name": d["name"], "mode": mode, "notes": d.get("notes", ""), "ok": ok,
                            "error": err[0] if err else "", "seconds": round(time.time() - t0, 2),
                            "_docx": str(docx), "_src": str(src), "_pages": d.get("pages")})
    # Render all DOCX files with one LibreOffice call per batch (much faster).
    docxs = [r["_docx"] for r in results if r["ok"]]
    for i in range(0, len(docxs), 20):
        # A private LibreOffice profile: does not clash with a running LibreOffice.
        subprocess.run(["soffice", f"-env:UserInstallation=file://{work}/lo-profile", "--headless",
                        "--convert-to", "pdf", "--outdir", str(work)] + docxs[i:i + 20],
                       capture_output=True, timeout=1800)
    for r in results:
        if not r["ok"]:
            continue
        conv_pdf = Path(r["_docx"]).with_suffix(".pdf")
        if not conv_pdf.exists():
            r.update(ok=False, error="LibreOffice could not render the DOCX")
            continue
        src, pages = Path(r["_src"]), r["_pages"]
        tag = f"{r['name']}.{r['mode']}"
        oi = render(src, out / "img", tag + ".orig", pages)
        ci = render(conv_pdf, out / "img", tag + ".conv")
        # Words a reader sees (headers and footers on every page, page-number fields
        # resolved) from the rendering; reading order from the document's logical text.
        orig_text = pdf_text(src, pages)
        f1, _ = text_scores(orig_text, pdf_text(conv_pdf))
        _, order = text_scores(orig_text, docx_text(r["_docx"]))
        po, pc = page_count(src, pages), page_count(conv_pdf)
        r.update(
            visual=visual_score(oi, ci),
            text_f1=f1,
            order=order,
            pages=round(min(po, pc) / max(po, pc), 4) if max(po, pc) else 1.0,
            pages_orig=po,
            pages_conv=pc,
            lines=line_breaks(src, conv_pdf, pages) if r["mode"] == "flow" else None,
            thumbs=[p.name for p in (oi[:2] + ci[:2])],
        )
        r["score"] = round(0.4 * r["visual"] + 0.4 * r["text_f1"] + 0.2 * r["pages"], 4)
    for r in results:
        for k in ("_docx", "_src", "_pages"):
            r.pop(k, None)
    (out / "results.json").write_text(json.dumps(results, indent=1, ensure_ascii=False))
    write_reports(results, out)
    shutil.rmtree(work, ignore_errors=True)
    bad = [r for r in results if not r["ok"]]
    print(f"{len(results)} conversions, {len(bad)} failed; report in {out}")
    for r in bad:
        print("  FAILED", r["name"], r["mode"], r["error"])


def write_reports(results, out):
    def avg(key, mode):
        v = [r[key] for r in results if r["mode"] == mode and r.get(key) is not None]
        return sum(v) / len(v) if v else 0.0

    modes = sorted({r["mode"] for r in results})
    md = ["# Conversion quality", "", "| mode | score | visual | text F1 | order | pages | lines |", "|---|---|---|---|---|---|---|"]
    for m in modes:
        md.append(f"| {m} | {avg('score', m):.3f} | {avg('visual', m):.3f} | {avg('text_f1', m):.3f} | {avg('order', m):.3f} | {avg('pages', m):.3f} | {avg('lines', m):.3f} |")
    md += ["", "| document | mode | score | visual | text F1 | order | pages | lines | s |", "|---|---|---|---|---|---|---|---|---|"]
    for r in results:
        if not r["ok"]:
            md.append(f"| {r['name']} | {r['mode']} | **failed** | {r['error']} | | | | | |")
            continue
        lines = "" if r["lines"] is None else f"{r['lines']:.2f}"
        md.append(f"| {r['name']} | {r['mode']} | {r['score']:.3f} | {r['visual']:.3f} | {r['text_f1']:.3f} | {r['order']:.3f} | "
                  f"{r['pages_conv']}/{r['pages_orig']} | {lines} | {r['seconds']} |")
    (out / "report.md").write_text("\n".join(md) + "\n")
    rows = []
    for r in results:
        if not r["ok"]:
            rows.append(f"<tr><td>{html.escape(r['name'])}</td><td>{r['mode']}</td><td colspan=3 class=bad>failed: {html.escape(r['error'])}</td></tr>")
            continue
        imgs = "".join(f"<img src='img/{t}' loading=lazy>" for t in r["thumbs"])
        rows.append(f"<tr><td>{html.escape(r['name'])}<br><small>{html.escape(r['notes'])}</small></td><td>{r['mode']}</td>"
                    f"<td>{r['score']:.3f}<br><small>visual {r['visual']:.2f} · text {r['text_f1']:.2f} · order {r['order']:.2f} · pages {r['pages_conv']}/{r['pages_orig']}</small></td><td>{imgs}</td></tr>")
    (out / "report.html").write_text(
        "<!doctype html><meta charset=utf-8><title>Conversion quality</title><style>body{font:14px sans-serif;margin:16px}"
        "td{border-top:1px solid #ccc;padding:6px;vertical-align:top}img{height:220px;margin:2px;border:1px solid #999}"
        ".bad{color:#b00}</style><h1>Conversion quality</h1><p>Thumbnails: original pages, then the converted DOCX rendered by LibreOffice.</p>"
        "<table>" + "".join(rows) + "</table>")


def cmd_compare(args):
    cur = {(r["name"], r["mode"]): r for r in json.loads(Path(args.results).read_text())}
    base = {(r["name"], r["mode"]): r for r in json.loads(Path(args.baseline).read_text())}
    problems = []
    for k, b in base.items():
        c = cur.get(k)
        if c is None:
            continue
        if b.get("ok") and not c.get("ok"):
            problems.append(f"{k}: now fails ({c.get('error')})")
        elif b.get("ok") and c.get("score", 0) < b.get("score", 0) - args.tolerance:
            problems.append(f"{k}: score {b['score']:.3f} → {c['score']:.3f}")
    better = [k for k, c in cur.items() if k in base and c.get("score", 0) > base[k].get("score", 0) + args.tolerance]
    print(f"{len(better)} improved, {len(problems)} regressed (tolerance {args.tolerance})")
    for p in problems:
        print("  REGRESSION", p)
    sys.exit(1 if problems else 0)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("fetch")
    r = sub.add_parser("run")
    r.add_argument("--bin", default=str(ROOT / "target" / "release" / "pdf2word"))
    r.add_argument("--modes", default="flow,exact")
    r.add_argument("--only")
    r.add_argument("--args", help="extra converter arguments, e.g. \"--no-lists\"")
    r.add_argument("--out", default=str(EVAL / "report"))
    c = sub.add_parser("compare")
    c.add_argument("results")
    c.add_argument("baseline")
    c.add_argument("--tolerance", type=float, default=0.03)
    args = ap.parse_args()
    {"fetch": cmd_fetch, "run": cmd_run, "compare": cmd_compare}[args.cmd](args)


if __name__ == "__main__":
    main()
