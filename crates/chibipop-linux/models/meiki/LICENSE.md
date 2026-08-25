# Bundled OCR models

These three ONNX files are the [meikiocr](https://github.com/rtr46/meikiocr)
text detector and its two character recognisers. chibipop's Linux adapter
runs them through ONNX Runtime; nothing else in the tree uses them.

| file | upstream repository |
|---|---|
| `meiki.text.detect.v0.1.960x544.onnx` | [`rtr46/meiki.text.detect.v0`](https://huggingface.co/rtr46/meiki.text.detect.v0) |
| `meiki.text.rec.v0.960x32.onnx` | [`rtr46/meiki.txt.recognition.v0`](https://huggingface.co/rtr46/meiki.txt.recognition.v0) |
| `meiki.text.rec.v0.vertical.32x480.onnx` | [`rtr46/meiki.txt.recognition.v0`](https://huggingface.co/rtr46/meiki.txt.recognition.v0) |

## Licence

The **model weights are LGPL-3.0** and are redistributed here unmodified, as
data files. That is compatible with chibipop's GPL-3.0-or-later: no source
obligation attaches beyond pointing at upstream, which the table above does.
meikiocr's own Python code is Apache-2.0 and is not vendored — the pipeline
was reimplemented in `../../src/ocr/`. ONNX Runtime is MIT.

## Why they are committed

ADR-0009: chibipop is offline-first and has **no first-run download path**,
so the models ship everywhere the binary does. `SHA256SUMS.txt` carries the
same digests as `tools/ocr-bench/models/SHA256SUMS.txt`, the benchmark that
chose this engine, and `src/ocr/models.rs` pins them in code and verifies
them when the engine opens. The quality gate in `tests/ocr_gate.rs` measured
*these bytes*; a different file is refused rather than quietly recognised
with.

Verify by hand with:

```bash
sha256sum -c SHA256SUMS.txt
```
