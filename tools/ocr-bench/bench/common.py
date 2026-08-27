"""Shared benchmark plumbing: normalization, CER, hit-scan, latency stats, image IO.

The text normalization mirrors chibipop's `src/text/layout.rs normalise()`
(kana followed by a hyphen-family char becomes 'ー'), preceded by NFKC and
whitespace stripping as the benchmark protocol in
docs/research/linux-japanese-ocr.md specifies.
"""

from __future__ import annotations

import json
import time
import unicodedata
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
MODELS = ROOT / "models"
CORPUS = ROOT / "corpus"
RESULTS = ROOT / "results"

# --------------------------------------------------------------------- text

_HYPHENS = {"-", "\u2010", "\u2013", "\u2014"}


def _is_kana(c: str) -> bool:
    return "\u3040" <= c <= "\u309f" or "\u30a0" <= c <= "\u30ff"


def normalise(text: str) -> str:
    """NFKC -> strip all whitespace -> chibipop normalise() hyphen rule."""
    text = unicodedata.normalize("NFKC", text)
    text = "".join(text.split())
    out: list[str] = []
    prev = None
    for c in text:
        if c in _HYPHENS and prev is not None and _is_kana(prev):
            c = "\u30fc"  # ー
        out.append(c)
        prev = c
    return "".join(out)


def levenshtein_ops(gt: str, pred: str) -> tuple[int, int, int]:
    """(substitutions, deletions, insertions) of the minimal edit script gt->pred."""
    m, n = len(gt), len(pred)
    # dp of (cost, subs, dels, ins) — cost primary, ops recovered by tie-broken DP.
    prev = [(j, 0, 0, j) for j in range(n + 1)]
    for i in range(1, m + 1):
        cur = [(i, 0, i, 0)] + [(0, 0, 0, 0)] * n
        gc = gt[i - 1]
        for j in range(1, n + 1):
            if gc == pred[j - 1]:
                cur[j] = prev[j - 1]
                continue
            sub = prev[j - 1]
            dele = prev[j]
            ins = cur[j - 1]
            best = min(
                (sub[0] + 1, sub[1] + 1, sub[2], sub[3]),
                (dele[0] + 1, dele[1], dele[2] + 1, dele[3]),
                (ins[0] + 1, ins[1], ins[2], ins[3] + 1),
            )
            cur[j] = best
        prev = cur
    _, s, d, i_ = prev[n]
    return s, d, i_


def cer(gt: str, pred: str) -> float:
    if not gt:
        return 0.0 if not pred else float(len(pred))
    s, d, i = levenshtein_ops(gt, pred)
    return (s + d + i) / len(gt)


# --------------------------------------------------------------------- geometry


@dataclass
class Box:
    """A recognized chunk with pixel geometry in crop coordinates."""

    text: str
    x: float
    y: float
    w: float
    h: float

    def contains(self, px: float, py: float) -> bool:
        return self.x <= px < self.x + self.w and self.y <= py < self.y + self.h

    @property
    def area(self) -> float:
        return max(self.w, 0.0) * max(self.h, 0.0)


@dataclass
class OcrOutput:
    """Uniform engine output: full text in reading order + chunk boxes."""

    text: str
    boxes: list[Box] = field(default_factory=list)


def hit_scan(gt_chars: list[dict], boxes: list[Box]) -> tuple[int, int]:
    """Simulate the cursor at each ground-truth char center.

    Success: the smallest engine box containing that point includes the char
    (after normalization). Returns (hits, total).
    """
    hits = 0
    total = 0
    for ch in gt_chars:
        c = normalise(ch["c"])
        if not c:  # whitespace ground truth is not hoverable
            continue
        total += 1
        px, py = ch["x"] + ch["w"] / 2.0, ch["y"] + ch["h"] / 2.0
        containing = [b for b in boxes if b.contains(px, py)]
        if not containing:
            continue
        best = min(containing, key=lambda b: b.area)
        if c in normalise(best.text):
            hits += 1
    return hits, total


def boxes_intersect(b: Box, rect: tuple[float, float, float, float]) -> bool:
    rx, ry, rw, rh = rect
    return not (b.x + b.w <= rx or rx + rw <= b.x or b.y + b.h <= ry or ry + rh <= b.y)


# --------------------------------------------------------------------- images


def load_bgra_bin(path: Path, w: int, h: int) -> np.ndarray:
    """Tightly packed 32bpp BGRA -> BGR ndarray (the production capture format)."""
    buf = np.fromfile(path, dtype=np.uint8)
    assert buf.size == w * h * 4, f"expected {w * h * 4} bytes, got {buf.size}"
    return buf.reshape(h, w, 4)[:, :, :3].copy()


def upscale_nn(img: np.ndarray, factor: int) -> np.ndarray:
    """Nearest-neighbour upscale, mirroring src/text/capture.rs upscale_by()."""
    return img.repeat(factor, axis=0).repeat(factor, axis=1)


# --------------------------------------------------------------------- timing


def time_call(fn) -> tuple[float, object]:
    t0 = time.perf_counter()
    out = fn()
    return (time.perf_counter() - t0) * 1000.0, out


def percentile(samples: list[float], p: float) -> float:
    if not samples:
        return float("nan")
    s = sorted(samples)
    k = (len(s) - 1) * p / 100.0
    f = int(k)
    c = min(f + 1, len(s) - 1)
    return s[f] + (s[c] - s[f]) * (k - f)


def latency_stats(samples: list[float]) -> dict:
    return {
        "n": len(samples),
        "p50_ms": round(percentile(samples, 50), 2),
        "p95_ms": round(percentile(samples, 95), 2),
        "mean_ms": round(sum(samples) / len(samples), 2) if samples else None,
    }


# --------------------------------------------------------------------- corpus IO


def load_manifest() -> dict:
    return json.loads((CORPUS / "manifest.json").read_text())


def load_crop(entry: dict) -> np.ndarray:
    import cv2

    img = cv2.imread(str(CORPUS / entry["file"]), cv2.IMREAD_COLOR)
    assert img is not None, entry["file"]
    return img


def peak_rss_mib() -> float:
    import resource

    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024.0
