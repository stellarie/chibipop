"""meikiocr det(960x544)+rec+vrec — candidate #1 (per-char boxes)."""

from __future__ import annotations

import numpy as np

from bench.common import MODELS, Box, OcrOutput

MEIKI_DIR = MODELS / "meiki"


class MeikiEngine:
    name = "meiki"
    has_boxes = True

    def __init__(self) -> None:
        import meikiocr.ocr as mocr

        # Point the package at the explicitly downloaded, hash-recorded models
        # instead of letting it re-fetch via hf_hub.
        mocr._get_model_path = lambda repo_id, filename: str(MEIKI_DIR / filename)
        self.ocr = mocr.MeikiOCR(provider="CPUExecutionProvider")

    def recognize(self, img: np.ndarray) -> OcrOutput:
        lines = self.ocr.run_ocr(img)
        # Reading order: horizontal lines top->bottom (detector pre-sorts by y),
        # vertical columns right->left.
        verticals = [l for l in lines if l["is_vertical"] and l["chars"]]
        horizontals = [l for l in lines if not l["is_vertical"] and l["chars"]]
        verticals.sort(key=lambda l: -l["chars"][0]["bbox"][0])
        ordered = horizontals + verticals
        boxes: list[Box] = []
        for line in ordered:
            for ch in line["chars"]:
                x1, y1, x2, y2 = ch["bbox"]
                boxes.append(Box(ch["char"], x1, y1, x2 - x1, y2 - y1))
        return OcrOutput("".join(l["text"] for l in ordered), boxes)

    def line_boxes(self, img: np.ndarray) -> list[list[int]]:
        """Detector-only pass: text-line bboxes [x1,y1,x2,y2] for manga-ocr feed (b)."""
        return [tb["bbox"] for tb in self.ocr.run_detection(img)]
