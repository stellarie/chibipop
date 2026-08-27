"""manga-ocr (mayocream ONNX export) via onnxruntime — candidate #4.

Text only, no geometry (excluded from hit-scan). Greedy and beam-k=4 decoding
over the raw encoder/decoder sessions; the decoder export carries no KV cache,
so each step re-runs the full prefix (noted in the report).
"""

from __future__ import annotations

import json

import cv2
import numpy as np

from bench.common import MODELS, OcrOutput

MDIR = MODELS / "manga-ocr"
MAX_STEPS = 300


class MangaEngine:
    has_boxes = False

    def __init__(self, decode: str = "greedy") -> None:  # "greedy" | "beam"
        import onnxruntime as ort

        self.name = f"manga-{decode}"
        self.decode = decode
        so = ort.SessionOptions()
        so.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        providers = ["CPUExecutionProvider"]
        self.enc = ort.InferenceSession(str(MDIR / "encoder_model.onnx"), so, providers=providers)
        self.dec = ort.InferenceSession(str(MDIR / "decoder_model.onnx"), so, providers=providers)
        cfg = json.loads((MDIR / "config.json").read_text())
        self.start_id = cfg.get("decoder_start_token_id", 2)
        self.eos_id = cfg.get("eos_token_id", 3)
        pp = json.loads((MDIR / "preprocessor_config.json").read_text())
        size = pp.get("size", 224)
        self.size = size["height"] if isinstance(size, dict) else size
        self.mean = np.array(pp.get("image_mean", [0.5, 0.5, 0.5]), np.float32)
        self.std = np.array(pp.get("image_std", [0.5, 0.5, 0.5]), np.float32)
        self.vocab = (MDIR / "vocab.txt").read_text().splitlines()
        self.special = {i for i, t in enumerate(self.vocab) if t.startswith("[") and t.endswith("]")}

    # ------------------------------------------------------------- pipeline

    def _preprocess(self, img: np.ndarray) -> np.ndarray:
        rgb = cv2.cvtColor(img, cv2.COLOR_BGR2RGB)
        rgb = cv2.resize(rgb, (self.size, self.size), interpolation=cv2.INTER_LINEAR)
        x = rgb.astype(np.float32) / 255.0
        x = (x - self.mean) / self.std
        return x.transpose(2, 0, 1)[None]

    def _logits(self, ids: list[int], hidden: np.ndarray) -> np.ndarray:
        out = self.dec.run(
            None,
            {
                "input_ids": np.array([ids], np.int64),
                "encoder_hidden_states": hidden,
            },
        )[0]
        return out[0, -1]

    def _greedy(self, hidden: np.ndarray) -> list[int]:
        ids = [self.start_id]
        for _ in range(MAX_STEPS):
            nxt = int(np.argmax(self._logits(ids, hidden)))
            ids.append(nxt)
            if nxt == self.eos_id:
                break
        return ids

    def _beam(self, hidden: np.ndarray, k: int = 4) -> list[int]:
        beams: list[tuple[list[int], float, bool]] = [([self.start_id], 0.0, False)]
        for _ in range(MAX_STEPS):
            live = [(i, b) for i, b in enumerate(beams) if not b[2]]
            if not live:
                break
            # All live beams share one prefix length: batch them into one call.
            batch = np.array([b[1][0] for b in live], np.int64)
            hid = np.repeat(hidden, len(live), axis=0)
            logits = self.dec.run(
                None, {"input_ids": batch, "encoder_hidden_states": hid}
            )[0][:, -1, :]
            mx = logits.max(axis=1, keepdims=True)
            logp = logits - (np.log(np.exp(logits - mx).sum(axis=1, keepdims=True)) + mx)
            cand: list[tuple[list[int], float, bool]] = [b for b in beams if b[2]]
            for row, (_, (ids, lp, _d)) in enumerate(live):
                top = np.argpartition(logp[row], -k)[-k:]
                for t in top:
                    cand.append((ids + [int(t)], lp + float(logp[row][t]), int(t) == self.eos_id))
            cand.sort(key=lambda b: b[1], reverse=True)
            beams = cand[:k]
        return max(beams, key=lambda b: b[1])[0]

    def _detok(self, ids: list[int]) -> str:
        toks = []
        for i in ids:
            if i in self.special or i >= len(self.vocab):
                continue
            t = self.vocab[i]
            toks.append(t[2:] if t.startswith("##") else t)
        return "".join(toks)

    def _run_one(self, img: np.ndarray) -> str:
        hidden = self.enc.run(None, {"pixel_values": self._preprocess(img)})[0]
        ids = self._greedy(hidden) if self.decode == "greedy" else self._beam(hidden)
        return self._detok(ids)

    # ------------------------------------------------------------- interface

    def recognize(self, img: np.ndarray) -> OcrOutput:
        """Feed (a): the whole crop in one pass."""
        return OcrOutput(self._run_one(img), [])

    def recognize_lines(self, img: np.ndarray, line_boxes: list[list[int]]) -> OcrOutput:
        """Feed (b): one pass per meikiocr-detected line box, joined in reading order."""
        if not line_boxes:
            return OcrOutput("", [])
        vertical = img.shape[0] > img.shape[1]
        order = sorted(line_boxes, key=(lambda b: -b[0]) if vertical else (lambda b: b[1]))
        parts = []
        h, w = img.shape[:2]
        for x1, y1, x2, y2 in order:
            x1, y1 = max(x1 - 2, 0), max(y1 - 2, 0)
            x2, y2 = min(x2 + 2, w), min(y2 + 2, h)
            if x2 <= x1 or y2 <= y1:
                continue
            parts.append(self._run_one(img[y1:y2, x1:x2]))
        return OcrOutput("".join(parts), [])
