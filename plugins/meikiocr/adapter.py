#!/usr/bin/env python3
"""chibipop text-provider adapter for meikiocr.

Protocol 1. Newline-delimited JSON on stdin and stdout.
Diagnostics go to stderr, which chibipop logs. Never to stdout.

Two methods:
  hello           -> construct MeikiOCR once, so the model load is paid here
  text/recognise  -> base64 PNG in, {"lines":[...]} out

Every rect is image-local, top-left origin, exactly as the wire wants.

Machine-specific paths (where meikiocr is installed, the HF cache, the
thread cap) live in config.toml, next to this file. Edit that, not this.
"""

import os
import sys


def _plugin_dir():
    return os.path.dirname(os.path.abspath(__file__))


def _read_config():
    """Flat `key = "value"` / `key = 123` reader for config.toml.

    Not tomllib: plugin.toml's `command` may name any Python on PATH, and
    tomllib only ships from 3.11. config.toml never nests a table or uses
    TOML escapes, so a line-by-line reader is all it needs.
    """
    cfg = {}
    path = os.path.join(_plugin_dir(), "config.toml")
    try:
        with open(path, "r", encoding="utf-8") as f:
            text = f.read()
    except OSError:
        return cfg
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line or "=" not in line:
            continue
        key, _, val = line.partition("=")
        key, val = key.strip(), val.strip()
        if len(val) >= 2 and val[0] == val[-1] and val[0] in "\"'":
            val = val[1:-1]
        elif val.isdigit():
            val = int(val)
        cfg[key] = val
    return cfg


_CFG = _read_config()

# meikiocr is normally a venv install, not on this interpreter's own
# sys.path. config.toml's meikiocr_path names the folder that holds the
# package (e.g. that venv's site-packages), so this makes it importable.
if _CFG.get("meikiocr_path"):
    sys.path.insert(0, _CFG["meikiocr_path"])

# HF settings must land before huggingface_hub is imported.
# HF_HUB_OFFLINE keeps a hover off the network once the cache is warm.
os.environ.setdefault(
    "HF_HOME", _CFG.get("hf_home") or os.path.join(_plugin_dir(), "hf-cache")
)
os.environ.setdefault("HF_HUB_OFFLINE", "1")
os.environ.setdefault("HF_HUB_DISABLE_TELEMETRY", "1")

import base64
import json
import time
import traceback

PROTOCOL = 1
NAME = "meikiocr"
VERSION = "0.1.0"

# Trap 6. 16 cores gave 65 ms, 4 cores gave 70 ms. 8% for 12 free cores.
# config.toml's threads overrides this default.
try:
    OCR_THREADS = int(_CFG.get("threads", 4))
except (TypeError, ValueError):
    OCR_THREADS = 4

_ocr = None
_cv2 = None
_np = None


def log(msg):
    """stderr only. stdout carries the protocol."""
    print(f"[meikiocr-adapter] {msg}", file=sys.stderr, flush=True)


def send(obj):
    """One line, ASCII-escaped, flushed. A buffered write deadlocks the host."""
    sys.stdout.write(json.dumps(obj, ensure_ascii=True) + "\n")
    sys.stdout.flush()


def cap_threads():
    """meikiocr exposes no thread knob, so preset the options it builds."""
    import onnxruntime as ort

    real = ort.SessionOptions

    def capped():
        o = real()
        o.intra_op_num_threads = OCR_THREADS
        o.inter_op_num_threads = 1
        return o

    ort.SessionOptions = capped


def load():
    """Build the engine. Called from hello, so the 1.3 s load is paid there."""
    global _ocr, _cv2, _np
    t0 = time.perf_counter()
    import logging
    lvl = logging.DEBUG if _CFG.get("debug") else logging.INFO
    logging.basicConfig(level=lvl, stream=sys.stderr,
                        format="[meikiocr] %(message)s")

    import cv2
    import numpy as np
    import meikiocr

    _cv2, _np = cv2, np
    cap_threads()
    _ocr = meikiocr.MeikiOCR(provider="CPUExecutionProvider")
    log(f"loaded in {time.perf_counter() - t0:.2f}s "
        f"provider={_ocr.active_provider} threads={OCR_THREADS}")

    # A cold first inference is slower. Pay it here, not on the first hover.
    try:
        t1 = time.perf_counter()
        _ocr.run_ocr(np.full((64, 64, 3), 255, dtype=np.uint8))
        log(f"warm-up {1000 * (time.perf_counter() - t1):.0f}ms")
    except Exception as e:  # A warm-up must never fail the handshake.
        log(f"warm-up skipped: {e}")


def as_bgr_u8(img):
    """Trap 2. meikiocr needs 3-channel uint8 BGR.

    4-channel raises ValueError. float32 returns [] and says nothing.
    Neither reaches run_ocr from here.
    """
    cv2, np = _cv2, _np
    if img.dtype != np.uint8:
        if np.issubdtype(img.dtype, np.floating):
            hi = float(img.max()) if img.size else 0.0
            img = np.clip(img * (255.0 if hi <= 1.0 else 1.0), 0, 255)
        elif img.dtype == np.uint16:
            img = img >> 8
        img = img.astype(np.uint8)
    if img.ndim == 2:
        img = cv2.cvtColor(img, cv2.COLOR_GRAY2BGR)
    elif img.shape[2] == 4:
        img = cv2.cvtColor(img, cv2.COLOR_BGRA2BGR)
    elif img.shape[2] == 1:
        img = cv2.cvtColor(img, cv2.COLOR_GRAY2BGR)
    if img.ndim != 3 or img.shape[2] != 3 or img.dtype != np.uint8:
        raise ValueError(f"cannot reach 3-channel uint8 BGR from "
                         f"shape {img.shape} dtype {img.dtype}")
    return np.ascontiguousarray(img)


def decode_png(b64):
    """base64 PNG to 3-channel uint8 BGR.

    IMREAD_COLOR is the fix for trap 2 at the source: it flattens alpha,
    expands grayscale, and downconverts 16-bit, all to uint8 BGR.
    """
    cv2, np = _cv2, _np
    raw = base64.b64decode(b64)
    flag = getattr(cv2, "IMREAD_COLOR_BGR", cv2.IMREAD_COLOR)
    img = cv2.imdecode(np.frombuffer(raw, dtype=np.uint8), flag)
    if img is None:
        raise ValueError("image_png did not decode as an image")
    return as_bgr_u8(img)


def order_band(row):
    """Horizontal reads left to right. Vertical Japanese reads right to left."""
    vertical = sum(1 for k in row if k[3].get("is_vertical")) * 2 > len(row)
    row.sort(key=lambda k: -k[1] if vertical else k[1])
    return row


def reading_order(lines):
    """Trap 4. meikiocr sorts by y1 alone, so columns interleave.

    Band lines whose tops agree, then order each band by x.
    """
    if len(lines) < 2:
        return lines
    keyed = []
    for ln in lines:
        boxes = [c["bbox"] for c in ln["chars"]]
        keyed.append((min(b[1] for b in boxes),
                      min(b[0] for b in boxes),
                      sorted(b[3] - b[1] for b in boxes)[len(boxes) // 2],
                      ln))
    band = max(8, sorted(k[2] for k in keyed)[len(keyed) // 2] // 2)
    keyed.sort(key=lambda k: k[0])
    out, row, top = [], [], keyed[0][0]
    for k in keyed:
        if k[0] - top > band:
            out.extend(order_band(row))
            row, top = [], k[0]
        row.append(k)
    out.extend(order_band(row))
    return [k[3] for k in out]


def recognise(params):
    """One wire Word per meikiocr character. bbox is [x1,y1,x2,y2]."""
    img = decode_png(params["image_png"])
    raw = _ocr.run_ocr(img)

    # Trap 3. A detected box whose chars all fail rec_threshold survives
    # with text == ''. len(raw) counts boxes, not readable lines.
    kept = [ln for ln in raw if ln.get("text") and ln.get("chars")]

    lines = []
    for ln in reading_order(kept):
        words = []
        for c in ln["chars"]:
            x1, y1, x2, y2 = c["bbox"]
            if not c.get("char"):
                continue
            words.append({
                "text": c["char"],
                # w = x2 - x1, h = y2 - y1. Image-local, so no offset.
                "rect": {"x": int(x1), "y": int(y1),
                         "w": max(1, int(x2) - int(x1)),
                         "h": max(1, int(y2) - int(y1))},
            })
        # is_vertical has no home in the wire. Protocol 1 says both sides
        # ignore an unknown field, so it rides along rather than dying.
        lines.append({"text": ln["text"], "words": words,
                      "is_vertical": bool(ln.get("is_vertical"))})
    return {"lines": lines}


def hello(_params):
    load()
    return {
        "protocol": PROTOCOL,
        "name": NAME,
        "version": VERSION,
        "roles": ["text-provider"],
        "features": [],
        "capabilities": {"geometry": True, "languages": ["ja"]},
    }


def main():
    # Windows pipes default to the locale codec. ensure_ascii already keeps
    # stdout safe; this makes stdin safe too.
    for s in (sys.stdin, sys.stdout):
        try:
            s.reconfigure(encoding="utf-8")
        except Exception:
            pass

    while True:
        line = sys.stdin.readline()
        if not line:
            break
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            log("ignoring a line that is not JSON")
            continue
        # Never validate strictly. An unknown field is not an error.
        rid = msg.get("id", 0)
        method = msg.get("method", "")
        params = msg.get("params") or {}
        try:
            if method == "hello":
                send({"id": rid, "result": hello(params)})
            elif method == "text/recognise":
                t0 = time.perf_counter()
                result = recognise(params)
                log(f"recognise {1000 * (time.perf_counter() - t0):.0f}ms "
                    f"{len(result['lines'])} line(s)")
                send({"id": rid, "result": result})
            else:
                # An unknown method is an error reply, never a crash.
                send({"id": rid, "error": f"unknown method \"{method}\""})
        except Exception as e:
            log(traceback.format_exc())
            send({"id": rid, "error": f"{type(e).__name__}: {e}"})
    return 0


if __name__ == "__main__":
    sys.exit(main())
