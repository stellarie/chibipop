"""Build the ground-truthed crop corpus.

1. Render render/wrapper.html (which embeds docs/fixtures/ocr-corpus.html
   untouched and adds two <=16 px small-glyph lines) with headless chromium:
   one pass for pixels, one for per-character DOM geometry via document.title.
2. Slice production-shaped crops: 500x100 horizontal, 100x500 vertical.
   Crops are trimmed to whole characters: anything past the last fully
   contained character is painted with the local background color, so the
   ground truth is exact (mirrors chibipop's clipped-word exclusion which
   drops partially captured glyphs anyway).
3. tests/fixtures/japanese_bgra.bin (exact production BGRA byte format) is the
   smoke case; its per-char boxes are recovered by ink-projection.
4. Every crop gets a 2x nearest-neighbour variant (src/text/capture.rs
   upscale_by). Masked variants per ADR-0008 are composited onto the 2x crops:
   positions edge/interior/outside-control x fills black/white/gray/mean x
   hard/1px-feather edges. Masked ground truth = chars not intersecting the mask.
"""

from __future__ import annotations

import html
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import cv2
import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from bench.common import CORPUS, ROOT, load_bgra_bin, upscale_nn  # noqa: E402

WRAPPER = ROOT / "render" / "wrapper.html"
FIXTURE = ROOT.parent.parent / "tests" / "fixtures" / "japanese_bgra.bin"
PAD = 5

HORIZONTAL = {
    "j1": "horizontal",
    "outlined": "horizontal",
    "j2": "horizontal",
    "alnum": "mixed",
    "s16": "small",
    "s12": "small",
}
VERTICAL = {"vert": "vertical"}
# Masked variants are generated for every 2x base crop (ADR-0008).
MASK_FILLS = ("black", "white", "gray", "mean")
MASK_EDGES = ("hard", "feather")


def chromium_bin() -> str:
    override = os.environ.get("OCR_BENCH_CHROMIUM")
    if override:
        return override
    for c in ("chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "chrome"):
        if shutil.which(c):
            return c
    raise SystemExit(
        "no chromium/chrome binary found on PATH; "
        "set OCR_BENCH_CHROMIUM to your browser binary"
    )


def render() -> tuple[np.ndarray, dict]:
    url = WRAPPER.resolve().as_uri()
    base = [
        chromium_bin(),
        "--headless=new",
        "--disable-gpu",
        "--hide-scrollbars",
        "--force-device-scale-factor=1",
        "--window-size=2560,1400",
        "--allow-file-access-from-files",
        "--virtual-time-budget=5000",
        "--default-background-color=FFFFFFFF",
    ]
    shot = CORPUS / "page.png"
    CORPUS.mkdir(exist_ok=True)
    subprocess.run(
        [*base, f"--screenshot={shot}", url],
        check=True, capture_output=True, cwd=ROOT,
    )
    dom = subprocess.run(
        [*base, "--dump-dom", url], check=True, capture_output=True, cwd=ROOT
    ).stdout.decode()
    m = re.search(r"<title>(.*?)</title>", dom, re.S)
    assert m, "wrapper produced no <title>"
    payload = html.unescape(m.group(1))
    assert payload.startswith("READY "), f"wrapper not ready: {payload[:80]!r}"
    geom = json.loads(payload[len("READY "):])
    img = cv2.imread(str(shot), cv2.IMREAD_COLOR)
    assert img is not None
    return img, geom


def slice_horizontal(page: np.ndarray, block: dict) -> tuple[np.ndarray, list[dict]]:
    """500x100 crop containing only whole GT glyphs.

    Rows above/below the text band are blanked to page white (they contain the
    corpus page's red debug labels, which are not part of any ground truth),
    and the area right of the last whole glyph is painted with the local line
    background (white for normal blocks, the dark panel color for `outlined`).
    """
    chars = block["chars"]
    x0 = chars[0]["x"] - PAD
    kept = [c for c in chars if c["x"] + c["w"] <= x0 + 500 - PAD]
    band_top = min(c["y"] for c in kept)
    band_bot = max(c["y"] + c["h"] for c in kept)
    y0 = band_top - 4
    cut = int(np.ceil(kept[-1]["x"] + kept[-1]["w"] - x0)) + 1
    crop = page[int(y0): int(y0) + 100, int(x0): int(x0) + 500].copy()
    # Local line background: 1px inside the crop at text-band center (inside
    # the dark panel for `outlined`, page white elsewhere).
    line_bg = crop[int((band_top + band_bot) / 2 - y0), 1].copy()
    crop[:, cut:] = line_bg
    # Line background outside the text band: removes the corpus page's red
    # label descenders above normal lines; extends the dark panel uniformly
    # for `outlined` instead of slicing it with white.
    top = max(int(band_top - y0), 0)
    bot = min(int(band_bot - y0) + 8, 100)
    crop[:top] = line_bg
    crop[bot:] = line_bg
    rel = [
        {"c": c["c"], "x": c["x"] - x0, "y": c["y"] - y0, "w": c["w"], "h": c["h"]}
        for c in kept
    ]
    return crop, rel


def slice_vertical(page: np.ndarray, block: dict) -> tuple[np.ndarray, list[dict]]:
    chars = block["chars"]
    y0 = chars[0]["y"] - PAD
    xc = chars[0]["x"] + chars[0]["w"] / 2.0
    x0 = xc - 50
    kept = [c for c in chars if c["y"] + c["h"] <= y0 + 500 - PAD]
    band_l = min(c["x"] for c in kept)
    band_r = max(c["x"] + c["w"] for c in kept)
    cut = int(np.ceil(kept[-1]["y"] + kept[-1]["h"] - y0)) + 1
    crop = page[int(y0): int(y0) + 500, int(x0): int(x0) + 100].copy()
    crop[cut:, :] = (255, 255, 255)
    left = max(int(band_l - x0) - 8, 0)
    right = min(int(band_r - x0) + 8, 100)
    crop[:, :left] = (255, 255, 255)
    crop[:, right:] = (255, 255, 255)
    # Blank any label sliver above the first glyph.
    top = max(int(kept[0]["y"] - y0), 0)
    crop[:top, :] = (255, 255, 255)
    rel = [
        {"c": c["c"], "x": c["x"] - x0, "y": c["y"] - y0, "w": c["w"], "h": c["h"]}
        for c in kept
    ]
    return crop, rel


def smoke_char_boxes(img: np.ndarray, n_expected: int) -> list[dict]:
    """Recover per-glyph boxes from clean dark-on-white ink by projection."""
    gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    ink = gray < 160
    cols = np.flatnonzero(ink.any(axis=0))
    assert cols.size, "no ink in smoke fixture"
    # Runs of consecutive ink columns.
    breaks = np.flatnonzero(np.diff(cols) > 1)
    runs = np.split(cols, breaks + 1)
    # Merge runs across the smallest gaps until n_expected remain.
    while len(runs) > n_expected:
        gaps = [runs[i + 1][0] - runs[i][-1] for i in range(len(runs) - 1)]
        i = int(np.argmin(gaps))
        runs[i: i + 2] = [np.concatenate(runs[i: i + 2])]
    boxes = []
    for run in runs:
        x0, x1 = int(run[0]), int(run[-1])
        rows = np.flatnonzero(ink[:, x0: x1 + 1].any(axis=1))
        boxes.append(
            {"x": x0, "y": int(rows[0]), "w": x1 - x0 + 1, "h": int(rows[-1] - rows[0] + 1)}
        )
    return boxes


def mask_rects(w: int, h: int, horizontal: bool) -> dict[str, tuple[float, float, float, float]]:
    """Popup-sized rects (~1/3 of crop area, popup-like proportions).

    edge: straddles one crop edge (extends past it, like a popup partially
    outside the capture strip); interior: fully inside; outside: adjacent but
    non-intersecting (pipeline no-op control).
    """
    if horizontal:
        return {
            "edge": (w - w / 3.0, -20, w / 3.0 + 120, h + 40),
            "interior": (w * 0.30, h * 0.10, w * 0.40, h * 0.80),
            "outside": (w + 10, -20, w / 3.0 + 120, h + 40),
        }
    return {
        "edge": (-20, h - h / 3.0, w + 40, h / 3.0 + 120),
        "interior": (w * 0.10, h * 0.30, w * 0.80, h * 0.40),
        "outside": (-20, h + 10, w + 40, h / 3.0 + 120),
    }


def composite_mask(img: np.ndarray, rect, fill: str, edge: str) -> np.ndarray:
    h, w = img.shape[:2]
    color = {
        "black": np.array([0, 0, 0], np.float32),
        "white": np.array([255, 255, 255], np.float32),
        "gray": np.array([128, 128, 128], np.float32),
        "mean": img.reshape(-1, 3).mean(axis=0).astype(np.float32),
    }[fill]
    x0, y0, rw, rh = rect
    x1, y1 = x0 + rw, y0 + rh
    cx0, cy0 = max(int(round(x0)), 0), max(int(round(y0)), 0)
    cx1, cy1 = min(int(round(x1)), w), min(int(round(y1)), h)
    out = img.copy()
    if cx0 >= cx1 or cy0 >= cy1:
        return out  # outside-control: no-op
    out[cy0:cy1, cx0:cx1] = color
    if edge == "feather":
        # 1px 50% ring on each border of the mask that lies inside the crop.
        ring = np.zeros((h, w), bool)
        if x0 >= 0:
            ring[cy0:cy1, cx0] = True
        if x1 <= w:
            ring[cy0:cy1, cx1 - 1] = True
        if y0 >= 0:
            ring[cy0, cx0:cx1] = True
        if y1 <= h:
            ring[cy1 - 1, cx0:cx1] = True
        blend = (img[ring].astype(np.float32) + color) / 2.0
        out[ring] = blend.astype(np.uint8)
    return out


def char_intersects(c: dict, rect) -> bool:
    x0, y0, rw, rh = rect
    return not (
        c["x"] + c["w"] <= x0 or x0 + rw <= c["x"] or c["y"] + c["h"] <= y0 or y0 + rh <= c["y"]
    )


def main() -> None:
    CORPUS.mkdir(exist_ok=True)
    page, geom = render()
    entries: list[dict] = []

    def emit(bid: str, slice_: str, crop: np.ndarray, chars: list[dict]) -> None:
        for scale in (1, 2):
            img = crop if scale == 1 else upscale_nn(crop, 2)
            sc = [
                {"c": c["c"], "x": c["x"] * scale, "y": c["y"] * scale,
                 "w": c["w"] * scale, "h": c["h"] * scale}
                for c in chars
            ]
            name = f"{bid}_{scale}x"
            cv2.imwrite(str(CORPUS / f"{name}.png"), img)
            entries.append({
                "id": name, "file": f"{name}.png", "base": bid, "slice": slice_,
                "scale": scale, "w": img.shape[1], "h": img.shape[0],
                "text": "".join(c["c"] for c in sc), "chars": sc, "mask": None,
            })
            if scale != 2:
                continue
            # ADR-0008 masked variants on the production (2x) shape.
            horizontal = img.shape[1] >= img.shape[0]
            rects = mask_rects(img.shape[1], img.shape[0], horizontal)
            variants = [("outside", "gray", "hard")] + [
                (pos, fill, edge)
                for pos in ("edge", "interior")
                for fill in MASK_FILLS
                for edge in MASK_EDGES
            ]
            for pos, fill, edge in variants:
                rect = rects[pos]
                m = composite_mask(img, rect, fill, edge)
                mname = f"{name}_m-{pos}-{fill}-{edge}"
                mchars = [c for c in sc if not char_intersects(c, rect)]
                cv2.imwrite(str(CORPUS / f"{mname}.png"), m)
                entries.append({
                    "id": mname, "file": f"{mname}.png", "base": bid, "slice": "masked",
                    "scale": 2, "w": m.shape[1], "h": m.shape[0],
                    "text": "".join(c["c"] for c in mchars), "chars": mchars,
                    "mask": {"pos": pos, "fill": fill, "edge": edge,
                             "rect": [round(v, 1) for v in rect], "unmasked": name},
                })

    for bid, slice_ in HORIZONTAL.items():
        crop, chars = slice_horizontal(page, geom[bid])
        emit(bid, slice_, crop, chars)
    for bid, slice_ in VERTICAL.items():
        crop, chars = slice_vertical(page, geom[bid])
        emit(bid, slice_, crop, chars)

    smoke = load_bgra_bin(FIXTURE, 400, 120)
    text = "昨日は"
    boxes = smoke_char_boxes(smoke, len(text))
    chars = [{"c": t, **b} for t, b in zip(text, boxes)]
    emit("smoke", "smoke", smoke, chars)

    (CORPUS / "manifest.json").write_text(
        json.dumps({"crops": entries}, ensure_ascii=False, indent=1)
    )
    n_masked = sum(1 for e in entries if e["mask"])
    print(f"corpus: {len(entries)} crops ({n_masked} masked) -> {CORPUS}")

    # Debug sheet: crops with GT boxes drawn, for eyeballing.
    dbg = CORPUS / "debug"
    dbg.mkdir(exist_ok=True)
    for e in entries:
        if e["mask"] or e["scale"] != 1:
            continue
        img = cv2.imread(str(CORPUS / e["file"]))
        for c in e["chars"]:
            cv2.rectangle(img, (int(c["x"]), int(c["y"])),
                          (int(c["x"] + c["w"]), int(c["y"] + c["h"])), (0, 0, 255), 1)
        cv2.imwrite(str(dbg / e["file"]), img)


if __name__ == "__main__":
    main()
