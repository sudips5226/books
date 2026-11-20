#!/usr/bin/env python3
"""Generates crates/core/data/metrics.bin: advance widths of Word's core fonts.

Widths come from metric-compatible open fonts (identical advance widths):
Liberation Serif/Sans/Mono = Times New Roman/Arial/Courier New,
Carlito = Calibri, Caladea = Cambria.

Format (little endian):
  b"P2WM" u16 version=1 u16 face_count
  per face: u8 name_len, name (utf-8), u8 style (bit0 bold, bit1 italic),
            u16 units_per_em, u16 default_advance, u32 entry_count,
            entry_count x (u32 codepoint, u16 advance)
Usage: python3 tools/gen_metrics.py [FONT_DIR...]
"""
import struct
import sys
from pathlib import Path

from fontTools.ttLib import TTFont

FACES = [
    ("Times New Roman", "LiberationSerif"),
    ("Arial", "LiberationSans"),
    ("Courier New", "LiberationMono"),
    ("Calibri", "Carlito"),
    ("Cambria", "Caladea"),
]
STYLES = [(0, "Regular"), (1, "Bold"), (2, "Italic"), (3, "BoldItalic")]
RANGES = [(0x20, 0x530), (0x2000, 0x2300), (0x2500, 0x2600), (0xFB00, 0xFB07)]

dirs = [Path(d) for d in sys.argv[1:]] or [Path("/usr/share/fonts")]


def find(stem):
    for d in dirs:
        for p in d.rglob(stem + ".ttf"):
            return p
    raise SystemExit(f"font {stem}.ttf not found")


out = bytearray(b"P2WM" + struct.pack("<HH", 1, len(FACES) * len(STYLES)))
for name, stem in FACES:
    for bits, style in STYLES:
        f = TTFont(find(f"{stem}-{style}"))
        cmap = f.getBestCmap()
        hmtx = f["hmtx"]
        upem = f["head"].unitsPerEm
        entries = []
        for lo, hi in RANGES:
            for cp in range(lo, hi):
                g = cmap.get(cp)
                if g is not None:
                    entries.append((cp, hmtx[g][0]))
        default = hmtx[cmap[ord("n")]][0]
        nb = name.encode()
        out += struct.pack("<B", len(nb)) + nb + struct.pack("<BHHI", bits, upem, default, len(entries))
        for cp, adv in entries:
            out += struct.pack("<IH", cp, adv)

dest = Path(__file__).resolve().parent.parent / "crates/core/data/metrics.bin"
dest.write_bytes(out)
print(f"wrote {dest} ({len(out)} bytes)")
