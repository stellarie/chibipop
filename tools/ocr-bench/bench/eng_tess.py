"""Tesseract 5 jpn/jpn_vert via the C API (ctypes) — candidate #3.

In-process C API keeps process-spawn out of the latency numbers (the harness
constraint). Symbol-level boxes via the result iterator (RIL_SYMBOL).
"""

from __future__ import annotations

import ctypes
import ctypes.util
from ctypes import c_char_p, c_int, c_void_p, byref
from pathlib import Path

import cv2
import numpy as np

from bench.common import MODELS, ROOT, Box, OcrOutput

RIL_WORD = 3
RIL_SYMBOL = 4
PSM_SINGLE_BLOCK_VERT_TEXT = 5
PSM_SINGLE_LINE = 7


def _load_lib() -> ctypes.CDLL:
    cands = [str(p) for p in sorted((ROOT / ".mamba" / "envs" / "tess" / "lib").glob("libtesseract.so*"))]
    found = ctypes.util.find_library("tesseract")  # system fallback, any distro layout
    if found:
        cands.append(found)
    for c in cands:
        try:
            return ctypes.CDLL(c)
        except OSError:
            continue
    raise RuntimeError("libtesseract not found; run setup.sh")


def _declare(lib: ctypes.CDLL) -> None:
    P, S, I = c_void_p, c_char_p, c_int
    IP = ctypes.POINTER(c_int)
    sig = {
        "TessBaseAPICreate": ([], P),
        "TessBaseAPIInit3": ([P, S, S], I),
        "TessBaseAPISetVariable": ([P, S, S], I),
        "TessBaseAPISetPageSegMode": ([P, I], None),
        "TessBaseAPISetImage": ([P, P, I, I, I, I], None),
        "TessBaseAPISetSourceResolution": ([P, I], None),
        "TessBaseAPIRecognize": ([P, P], I),
        "TessBaseAPIGetUTF8Text": ([P], P),
        "TessBaseAPIGetIterator": ([P], P),
        "TessBaseAPIEnd": ([P], None),
        "TessBaseAPIDelete": ([P], None),
        "TessDeleteText": ([P], None),
        "TessResultIteratorGetUTF8Text": ([P, I], P),
        "TessResultIteratorGetPageIterator": ([P], P),
        "TessResultIteratorNext": ([P, I], I),
        "TessResultIteratorDelete": ([P], None),
        "TessPageIteratorBoundingBox": ([P, I, IP, IP, IP, IP], I),
    }
    for name, (argtypes, restype) in sig.items():
        fn = getattr(lib, name)
        fn.argtypes = argtypes
        fn.restype = restype


class _Api:
    def __init__(self, lib: ctypes.CDLL, datapath: Path, lang: str, psm: int,
                 level: int = RIL_SYMBOL) -> None:
        self.lib = lib
        self.h = lib.TessBaseAPICreate()
        rc = lib.TessBaseAPIInit3(self.h, str(datapath).encode(), lang.encode())
        if rc != 0:
            raise RuntimeError(f"tesseract Init3 failed for {lang} in {datapath}")
        lib.TessBaseAPISetVariable(self.h, b"preserve_interword_spaces", b"1")
        lib.TessBaseAPISetPageSegMode(self.h, psm)
        self.level = level

    def recognize(self, gray: np.ndarray) -> OcrOutput:
        lib = self.lib
        h, w = gray.shape
        buf = np.ascontiguousarray(gray)
        lib.TessBaseAPISetImage(
            self.h, buf.ctypes.data_as(c_void_p), w, h, 1, w
        )
        lib.TessBaseAPISetSourceResolution(self.h, 192)
        if lib.TessBaseAPIRecognize(self.h, None) != 0:
            return OcrOutput("", [])
        tptr = lib.TessBaseAPIGetUTF8Text(self.h)
        text = ctypes.cast(tptr, c_char_p).value.decode("utf-8") if tptr else ""
        if tptr:
            lib.TessDeleteText(c_void_p(tptr))
        boxes: list[Box] = []
        it = lib.TessBaseAPIGetIterator(self.h)
        if it:
            pi = lib.TessResultIteratorGetPageIterator(c_void_p(it))
            lvl = self.level
            while True:
                sptr = lib.TessResultIteratorGetUTF8Text(c_void_p(it), lvl)
                if sptr:
                    sym = ctypes.cast(sptr, c_char_p).value.decode("utf-8")
                    lib.TessDeleteText(c_void_p(sptr))
                    l, t, r, b = c_int(), c_int(), c_int(), c_int()
                    if lib.TessPageIteratorBoundingBox(
                        c_void_p(pi), lvl, byref(l), byref(t), byref(r), byref(b)
                    ):
                        boxes.append(Box(sym, l.value, t.value, r.value - l.value, b.value - t.value))
                if not lib.TessResultIteratorNext(c_void_p(it), lvl):
                    break
            lib.TessResultIteratorDelete(c_void_p(it))
        return OcrOutput(text, boxes)

    def close(self) -> None:
        self.lib.TessBaseAPIEnd(self.h)
        self.lib.TessBaseAPIDelete(self.h)


class TessEngine:
    """Orientation-routed pair: jpn+psm7 for horizontal crops, jpn_vert+psm5 for vertical."""

    has_boxes = True

    def __init__(self, variant: str) -> None:  # variant: "fast" | "best"
        self.name = f"tess-{variant}"
        lib = _load_lib()
        _declare(lib)
        datapath = MODELS / "tessdata" / variant
        self.h_api = _Api(lib, datapath, "jpn", PSM_SINGLE_LINE)
        # jpn_vert symbol-level boxes come back degenerate (x=0, w=0 — measured;
        # see the report's Tesseract caveats). Word level is the best usable
        # geometry for vertical text, so hit-scan gets Tesseract's best case.
        self.v_api = _Api(lib, datapath, "jpn_vert", PSM_SINGLE_BLOCK_VERT_TEXT, level=RIL_WORD)

    def recognize(self, img: np.ndarray) -> OcrOutput:
        gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
        vertical = img.shape[0] > img.shape[1]
        return (self.v_api if vertical else self.h_api).recognize(gray)
