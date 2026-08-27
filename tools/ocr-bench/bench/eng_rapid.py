"""PP-OCRv5 mobile det+rec via RapidOCR's ONNX distribution — candidate #2.

The Rust plan is `oar-ocr`/own `ort` pipeline; here the RapidOCR Python
wrapper drives the exact same ONNX files. `return_word_box=True` exercises the
word-box path whose fidelity is a named benchmark question.
"""

from __future__ import annotations

import numpy as np

from bench.common import MODELS, Box, OcrOutput

PDIR = MODELS / "ppocrv5"


class RapidEngine:
    name = "ppocrv5"
    has_boxes = True

    def __init__(self) -> None:
        from rapidocr import EngineType, ModelType, OCRVersion, RapidOCR

        self.ocr = RapidOCR(
            params={
                "Det.engine_type": EngineType.ONNXRUNTIME,
                "Det.ocr_version": OCRVersion.PPOCRV5,
                "Det.model_type": ModelType.MOBILE,
                "Det.model_path": str(PDIR / "ch_PP-OCRv5_det_mobile.onnx"),
                "Rec.engine_type": EngineType.ONNXRUNTIME,
                "Rec.ocr_version": OCRVersion.PPOCRV5,
                "Rec.model_type": ModelType.MOBILE,
                "Rec.model_path": str(PDIR / "ch_PP-OCRv5_rec_mobile.onnx"),
                "Rec.rec_keys_path": str(PDIR / "ppocrv5_dict.txt"),
            }
        )

    def recognize(self, img: np.ndarray) -> OcrOutput:
        res = self.ocr(img, use_det=True, use_cls=False, use_rec=True, return_word_box=True)
        if res is None or res.txts is None:
            return OcrOutput("", [])
        vertical = img.shape[0] > img.shape[1]
        lines = list(zip(res.boxes, res.txts, res.word_results or [() for _ in res.txts]))
        # Reading order: horizontal top->bottom, vertical columns right->left.
        if vertical:
            lines.sort(key=lambda l: -float(np.min(np.asarray(l[0])[:, 0])))
        else:
            lines.sort(key=lambda l: float(np.min(np.asarray(l[0])[:, 1])))
        text = "".join(t for _, t, _ in lines)
        boxes: list[Box] = []
        for quad, _txt, words in lines:
            for w in words:
                # word_results entries: (word_text, score, word_quad 4x2) — quad may
                # be None when the wrapper cannot re-project.
                if len(w) < 3 or w[2] is None:
                    continue
                q = np.asarray(w[2], dtype=np.float64)
                x0, y0 = q[:, 0].min(), q[:, 1].min()
                boxes.append(Box(str(w[0]), x0, y0, q[:, 0].max() - x0, q[:, 1].max() - y0))
        return OcrOutput(text, boxes)
