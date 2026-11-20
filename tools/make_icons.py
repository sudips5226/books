#!/usr/bin/env python3
"""Draws the application icon and writes PNG, ICO and ICNS variants to packaging/icons."""
import os
from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "packaging", "icons")
S = 1024


def font(size):
    for p in ["/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf"]:
        if os.path.exists(p):
            return ImageFont.truetype(p, size)
    return ImageFont.load_default()


img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
# Rounded square background (teal).
d.rounded_rectangle((40, 40, S - 40, S - 40), radius=200, fill=(17, 122, 120, 255))


def page(x, y, w, h, fold=90):
    d.polygon([(x, y), (x + w - fold, y), (x + w, y + fold), (x + w, y + h), (x, y + h)], fill=(255, 255, 255, 255))
    d.polygon([(x + w - fold, y), (x + w - fold, y + fold), (x + w, y + fold)], fill=(205, 225, 224, 255))


# Source PDF page (back, left) with a red label.
page(150, 150, 400, 520)
d.rounded_rectangle((110, 230, 400, 340), radius=24, fill=(214, 64, 52, 255))
d.text((255, 287), "PDF", font=font(84), fill="white", anchor="mm")
for y in (400, 450, 500, 550):
    d.rounded_rectangle((200, y, 480, y + 18), radius=9, fill=(200, 205, 210, 255))
# Editable document (front, right) with text lines and a cursor.
page(460, 360, 400, 520)
for i, y in enumerate((470, 530, 590, 650, 710, 770)):
    d.rounded_rectangle((510, y, 810 - (i % 3) * 50, y + 22), radius=11, fill=(17, 122, 120, 255))
d.rectangle((775, 640, 787, 700), fill=(240, 160, 30, 255))
# Arrow from the PDF to the document.
d.polygon([(420, 700), (520, 700), (520, 660), (600, 740), (520, 820), (520, 780), (420, 780)], fill=(240, 160, 30, 255))

os.makedirs(OUT, exist_ok=True)
img.resize((512, 512), Image.LANCZOS).save(os.path.join(OUT, "pdf2word.png"))
img.save(os.path.join(OUT, "pdf2word.ico"), sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
img.save(os.path.join(OUT, "pdf2word.icns"))
print("icons written to", os.path.normpath(OUT))
