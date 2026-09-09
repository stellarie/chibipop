#!/usr/bin/env python3
"""Render the large-text screens under crates/chibipop-linux/tests/fixtures/large-text/.

The OCR gate reads these screens through `TextSource` with the real engine. They
model issue #92: text at or above the height of the 500x100 capture box. The
corpus under tests/fixtures/ocr-corpus/ stays the benchmark's, unchanged.

The output is committed. Run this script only to change a screen. It needs Pillow
and the Noto Sans CJK font at FONT. A different font or version changes the ink
boxes in manifest.json, so commit the PNGs and the manifest together.
"""
import json
import os
import sys

from PIL import Image, ImageDraw, ImageFont

FONT = "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"
# Index 0 of the Noto CJK collection is the JP face.
FONT_INDEX = 0
OUT = os.path.join(os.path.dirname(__file__), "..", "crates", "chibipop-linux", "tests", "fixtures", "large-text")
SCREEN = (1280, 720)

# (id, text, glyph size, x of the first glyph, baseline y, index of the hovered glyph)
SCREENS = [
    ("shinki_100", "新規", 100, 400, 400, 0),
    ("shinki_130", "新規", 130, 400, 420, 0),
    ("shinki_160", "新規", 160, 400, 440, 0),
    ("nihongo_130", "日本語を話す", 130, 200, 420, 2),
]


def render(id_, text, size, x0, baseline, hovered):
    font = ImageFont.truetype(FONT, size, index=FONT_INDEX)
    screen = Image.new("RGB", SCREEN, "white")
    draw = ImageDraw.Draw(screen)
    draw.text((x0, baseline), text, font=font, fill="black", anchor="ls")
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
        "size": size,
        "text": text,
        "chars": chars,
        "hover": {"x": hit["x"] + hit["w"] // 2, "y": hit["y"] + hit["h"] // 2},
        "expect": text[hovered:],
    }


def main():
    os.makedirs(OUT, exist_ok=True)
    manifest = {
        "font": "Noto Sans CJK JP Regular",
        "screen": {"w": SCREEN[0], "h": SCREEN[1]},
        "screens": [render(*screen) for screen in SCREENS],
    }
    with open(os.path.join(OUT, "manifest.json"), "w", encoding="utf-8") as f:
        json.dump(manifest, f, ensure_ascii=False, indent=1)
        f.write("\n")
    print(f"wrote {len(SCREENS)} screens to {os.path.normpath(OUT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
