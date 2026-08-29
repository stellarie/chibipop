"""Regenerate the checked-in media fixtures.

Every asset here is generated, never copied out of a real dictionary: the
probe in `src/dict/media.rs` reads container headers, so what it needs is
one file per format with a *known* size, and a generated file's size is
known by construction. Run from the repository root:

    python tests/fixtures/media/make_fixtures.py

Needs ImageMagick (`magick`) and `avifenc` on PATH. Both are packaging
tools, not build dependencies - the outputs are committed.

The sizes are deliberately all different and none of them square, so a
probe that reads the right bytes off the wrong format, or swaps width and
height, fails instead of passing quietly.
"""

import json
import shutil
import struct
import subprocess
import zipfile
from pathlib import Path

HERE = Path(__file__).parent

# 12x7 true-colour RGBA at half alpha: the straightforward decode path, and
# the only fixture whose premultiplied pixels are worth asserting.
ONE_PNG = ("one.png", ["-size", "12x7", "xc:rgba(0,0,255,0.5)", "PNG32:{out}"])
# 5x4 grey plus alpha: `png`'s EXPAND does not widen grey to RGB, so this is
# the fixture that covers the decoder's own lane widening.
GREY_PNG = (
    "grey.png",
    ["-size", "5x4", "-colorspace", "Gray", "xc:gray(64)", "-alpha", "set",
     "-channel", "A", "-evaluate", "set", "50%", "PNG00:{out}"],
)
# 3x3 one-bit palette. Referenced by nothing, which is what makes it the
# proof that an unreferenced asset is not extracted.
UNUSED_PNG = ("unused.png", ["-size", "3x3", "xc:#204020", "{out}"])
THREE_JPG = ("three.jpg", ["-size", "23x11", "xc:#a05030", "-quality", "80", "{out}"])
# Referenced only by an image-only term row, which renders no text and is
# therefore not an entry at all - so this asset is unreachable too.
DROPPED_PNG = ("dropped.png", ["-size", "6x6", "xc:#402040", "{out}"])

MAGICK = [ONE_PNG, GREY_PNG, UNUSED_PNG, THREE_JPG, DROPPED_PNG]

# Two frames of 4x3 inside a 9x5 logical screen. ImageMagick will not write
# a canvas bigger than its frames, so the logical screen descriptor is
# patched afterwards - which is exactly the real shape an animation has, and
# the logical screen is the intrinsic size a browser uses for it.
GIF_CANVAS = (9, 5)

# `width`/`height` present, one per line, in the attribute order and the
# whitespace style Inkscape writes - which is the shape the census's SVG
# gaiji actually arrive in.
TWO_SVG = """<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<svg
   height="32"
   width="64"
   viewBox="0 0 128 64"
   version="1.1"
   xmlns="http://www.w3.org/2000/svg">
  <rect x="8" y="8" width="112" height="48" fill="#3050a0" />
</svg>
"""

# No `width`, no `height`: the size *is* the viewBox, which is the common
# shape for a gaiji drawn from a font glyph.
RATIO_SVG = """<svg viewBox="0 0 100 40" xmlns="http://www.w3.org/2000/svg">
  <path d="M 10,10 H 90 V 30 H 10 Z" fill="#a05030" />
</svg>
"""

INDEX = {"title": "FixtureMedia", "format": 3, "revision": "1"}

# Row 5 is an image-only glossary. It renders no text, so it is not an entry
# and `gaiji/dropped.png` is referenced by nothing the popup can reach.
TERM_BANK = [
    ["いぬ", "いぬ", "", "", 0, [{"type": "structured-content", "content": [
        {"tag": "img", "path": "gaiji/one.png", "height": 1.0, "sizeUnits": "em"},
        {"tag": "img", "path": "gaiji/copy.png", "height": 1.0, "sizeUnits": "em"},
        {"tag": "span", "content": "dog"},
    ]}]],
    ["ねこ", "ねこ", "", "", 0, [{"type": "structured-content", "content": [
        {"tag": "img", "path": "gaiji/three.jpg"},
        {"tag": "img", "path": "gaiji/four.gif"},
        {"tag": "img", "path": "gaiji/five.avif"},
        {"tag": "span", "content": "cat"},
    ]}]],
    ["とり", "とり", "", "", 0, [{"type": "structured-content", "content": [
        {"tag": "img", "path": "gaiji/two.svg"},
        {"tag": "img", "path": "gaiji/ratio.svg"},
        {"tag": "span", "content": "bird"},
    ]}]],
    ["さかな", "さかな", "", "", 0, [{"type": "structured-content", "content": [
        {"tag": "img", "path": "gaiji/missing.png", "data": {"alt": "[fish]"}},
        {"tag": "img", "path": "gaiji/broken.png"},
        {"tag": "span", "content": "fish"},
    ]}]],
    ["かめ", "かめ", "", "", 0, [{"type": "image", "path": "gaiji/dropped.png"}]],
]

# `copy.png` is `one.png`'s bytes at a second path: two media rows, one
# blob. `broken.png` is a PNG signature with no IHDR behind it - present,
# sniffed, and unsizeable, which is the skip-with-a-diagnostic case.
# `gaiji/missing.png` is referenced and absent on purpose.
ARCHIVE_MEDIA = {
    "gaiji/one.png": "one.png",
    "gaiji/copy.png": "one.png",
    "gaiji/three.jpg": "three.jpg",
    "gaiji/four.gif": "four.gif",
    "gaiji/five.avif": "five.avif",
    "gaiji/two.svg": "two.svg",
    "gaiji/ratio.svg": "ratio.svg",
    "gaiji/broken.png": "broken.png",
    "gaiji/unused.png": "unused.png",
    "gaiji/dropped.png": "dropped.png",
}


def run(args):
    subprocess.run(args, check=True, capture_output=True)


def rasters():
    for name, argv in MAGICK:
        out = str(HERE / name)
        run(["magick"] + [a.format(out=out) for a in argv])

    frames = HERE / "four.gif"
    run(["magick", "-size", "4x3", "xc:red", "-size", "4x3", "xc:blue",
         "-delay", "10", "-loop", "0", str(frames)])
    raw = bytearray(frames.read_bytes())
    raw[6:10] = struct.pack("<HH", *GIF_CANVAS)
    frames.write_bytes(raw)

    # 480x120, and wide on purpose: the one format whose size lives in an
    # item property rather than a header.
    plain = HERE / "_plain.png"
    run(["magick", "-size", "480x120", "gradient:red-blue", str(plain)])
    run(["avifenc", "-q", "40", "-s", "10", str(plain), str(HERE / "five.avif")])
    plain.unlink()

    (HERE / "two.svg").write_text(TWO_SVG, encoding="utf-8")
    (HERE / "ratio.svg").write_text(RATIO_SVG, encoding="utf-8")
    # A PNG signature and four bytes of nothing: sniffs as a PNG, holds no
    # IHDR.
    (HERE / "broken.png").write_bytes(b"\x89PNG\r\n\x1a\n\x00\x00\x00\x00")


def archive():
    with zipfile.ZipFile(HERE / "media.zip", "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("index.json", json.dumps(INDEX, ensure_ascii=False))
        z.writestr("term_bank_1.json", json.dumps(TERM_BANK, ensure_ascii=False))
        for inside, source in ARCHIVE_MEDIA.items():
            z.writestr(inside, (HERE / source).read_bytes())


def bomb():
    """A separate archive holding one asset far over the extraction cap.

    A PNG signature and 17 MiB of zeros: it sniffs as a PNG, it declares an
    uncompressed size the build must refuse before reading, and it deflates
    to a few hundred bytes - which is precisely the shape the cap exists
    for. Its own archive, so `media.zip`'s counts stay readable.
    """
    with zipfile.ZipFile(HERE / "oversized.zip", "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("index.json", json.dumps({"title": "FixtureOversized"}))
        z.writestr("gaiji/huge.png", b"\x89PNG\r\n\x1a\n" + bytes((17 << 20) - 8))


if __name__ == "__main__":
    if shutil.which("magick") is None or shutil.which("avifenc") is None:
        raise SystemExit("needs ImageMagick and avifenc on PATH")
    rasters()
    archive()
    bomb()
    print(f"wrote {HERE}/media.zip, {HERE}/oversized.zip and their assets")
