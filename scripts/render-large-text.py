#!/usr/bin/env python3
"""Render the large-text screens under crates/chibipop-linux/tests/fixtures/large-text/.

The OCR gate reads these screens through `TextSource` with the real engine. They
model issue #92: text at or above the height of the 500x100 capture box, dark on
light and light on dark. The corpus under tests/fixtures/ocr-corpus/ stays the
benchmark's, unchanged.

The output is committed. Run this script only to change a screen. It needs Pillow
and two fonts:

- Noto Sans CJK JP at NOTO. Most distributions package it.
- BIZ UDPGothic, the font of the issue #92 follow-up screenshot. It is OFL, from
  https://github.com/googlefonts/morisawa-biz-ud-gothic (fonts/ttf/). Point
  BIZ_UDPGOTHIC_DIR at a directory that holds BIZUDPGothic-Regular.ttf and
  BIZUDPGothic-Bold.ttf.

A different font or version changes the ink boxes in manifest.json, so commit the
PNGs and the manifest together.
"""
import json
import os
import sys

from PIL import Image, ImageDraw, ImageFont

NOTO = "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"
BIZ_DIR = os.environ.get("BIZ_UDPGOTHIC_DIR", "/usr/share/fonts/TTF")
FONTS = {
    # Index 0 of the Noto CJK collection is the JP face.
    "noto": (NOTO, 0),
    "biz": (os.path.join(BIZ_DIR, "BIZUDPGothic-Regular.ttf"), 0),
    "biz-bold": (os.path.join(BIZ_DIR, "BIZUDPGothic-Bold.ttf"), 0),
}
OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "chibipop-linux", "tests", "fixtures", "large-text")
SCREEN = (1280, 720)

# One row per screen:
# id, font, text, glyph size, x of the first glyph, baseline y, hovered glyph index,
# foreground, background, expected text after the hovered glyph, and whether the
# expected text is only a prefix (the box holds part of a long line).
SCREENS = [
    ("shinki_100", "noto", "新規", 100, 400, 400, 0, "black", "white", None, False),
    ("shinki_130", "noto", "新規", 130, 400, 420, 0, "black", "white", None, False),
    ("shinki_160", "noto", "新規", 160, 400, 440, 0, "black", "white", None, False),
    ("nihongo_130", "noto", "日本語を話す", 130, 200, 420, 2, "black", "white", None, False),
    # The follow-up screenshot: a news line in BIZ UDPGothic, white on black.
    ("katsu_biz_100", "biz", "活発な秋雨前線や低気圧の影", 100, 120, 460, 0, "white", "black", "活発な", True),
    ("katsu_biz_bold_125", "biz-bold", "活発な秋雨前線や低気圧の影", 125, 120, 470, 0, "white", "black", "活発な", True),
    ("katsu_noto_100", "noto", "活発な秋雨前線や低気圧の影", 100, 120, 460, 0, "white", "black", "活発な", True),
]


def render(id_, face, text, size, x0, baseline, hovered, fg, bg, expect, prefix):
    path, index = FONTS[face]
    font = ImageFont.truetype(path, size, index=index)
    screen = Image.new("RGB", SCREEN, bg)
    draw = ImageDraw.Draw(screen)
    draw.text((x0, baseline), text, font=font, fill=fg, anchor="ls")
    chars = []
    x = x0
    for ch in text:
        left, top, right, bottom = draw.textbbox((x, baseline), ch, font=font, anchor="ls")
        chars.append({"c": ch, "x": int(left), "y": int(top), "w": int(right - left), "h": int(bottom - top)})
        x += int(round(font.getlength(ch)))
    screen.save(os.path.join(OUT, f"{id_}.png"))
    hit = chars[hovered]
    return {
        "id": id_,
        "file": f"{id_}.png",
        "font": face,
        "size": size,
        "text": text,
        "chars": chars,
        "hover": {"x": hit["x"] + hit["w"] // 2, "y": hit["y"] + hit["h"] // 2},
        "expect": expect if expect is not None else text[hovered:],
        "prefix": prefix,
    }


def main():
    os.makedirs(OUT, exist_ok=True)
    manifest = {
        "fonts": {"noto": "Noto Sans CJK JP Regular", "biz": "BIZ UDPGothic Regular", "biz-bold": "BIZ UDPGothic Bold"},
        "screen": {"w": SCREEN[0], "h": SCREEN[1]},
        "screens": [render(*screen) for screen in SCREENS],
    }
    with open(os.path.join(OUT, "manifest.json"), "w", encoding="utf-8") as f:
        json.dump(manifest, f, ensure_ascii=False, indent=1)
        f.write("\n")
    print(f"wrote {len(SCREENS)} screens to {os.path.normpath(OUT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
