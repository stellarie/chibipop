#!/usr/bin/env bash
# One-shot environment + model setup for the chibipop OCR benchmark.
# Everything it creates lives under tools/ocr-bench/ — no host writes, no sudo.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=env.sh
source "$ROOT/env.sh"
cd "$ROOT"

mkdir -p models/tessdata/fast models/tessdata/best models/meiki models/ppocrv5 \
         models/manga-ocr .cache bin results corpus

# ---------------------------------------------------------------- python venv
if [ ! -x .venv/bin/python ]; then
    python3 -m venv .venv
fi
.venv/bin/pip install --quiet --upgrade pip
# rapidocr declares opencv-python (GUI build); meikiocr declares the headless
# build. Both ship the same `cv2` package. Install everything, then force the
# headless build back on top so the venv needs no GUI libs.
.venv/bin/pip install --quiet \
    numpy opencv-python-headless onnxruntime pillow \
    meikiocr==0.3.4 rapidocr==3.9.2
.venv/bin/pip install --quiet --force-reinstall --no-deps opencv-python-headless

# ------------------------------------------------- tesseract (user-space, conda-forge)
# No system tesseract and no sudo: pull the binary + libtesseract from
# conda-forge into a micromamba prefix INSIDE this directory. Never `shell init`.
if [ ! -x bin/micromamba ]; then
    case "$(uname -m)" in
        x86_64)  MAMBA_PLATFORM=linux-64 ;;
        aarch64) MAMBA_PLATFORM=linux-aarch64 ;;
        *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;;
    esac
    curl -Ls "https://micro.mamba.pm/api/micromamba/$MAMBA_PLATFORM/latest" \
        | tar -xj -C "$ROOT" bin/micromamba
fi
if [ ! -x .mamba/envs/tess/bin/tesseract ]; then
    bin/micromamba create -y -q -p "$ROOT/.mamba/envs/tess" -c conda-forge tesseract
fi

# ---------------------------------------------------------------------- models
SUMS="$ROOT/models/SHA256SUMS.txt"
: > "$SUMS"
fetch() { # fetch <url> <dest>
    local url="$1" dest="$2"
    if [ ! -s "$dest" ]; then
        echo "fetching $dest"
        curl -L --fail --retry 3 -o "$dest.part" "$url"
        mv "$dest.part" "$dest"
    fi
    sha256sum "$dest" >> "$SUMS"
}

# Tesseract traineddata — exact sources from the research doc.
TD=https://github.com/tesseract-ocr
fetch "$TD/tessdata_fast/raw/main/jpn.traineddata"      models/tessdata/fast/jpn.traineddata
fetch "$TD/tessdata_fast/raw/main/jpn_vert.traineddata" models/tessdata/fast/jpn_vert.traineddata
fetch "$TD/tessdata_best/raw/main/jpn.traineddata"      models/tessdata/best/jpn.traineddata
fetch "$TD/tessdata_best/raw/main/jpn_vert.traineddata" models/tessdata/best/jpn_vert.traineddata

# meikiocr models (HF repos cited in the research doc). Downloaded explicitly so
# the benchmark records their hashes; eng_meiki.py points the package at these
# files instead of letting it re-download via hf_hub.
MHF=https://huggingface.co
fetch "$MHF/rtr46/meiki.text.detect.v0/resolve/main/meiki.text.detect.v0.1.960x544.onnx" \
      models/meiki/meiki.text.detect.v0.1.960x544.onnx
fetch "$MHF/rtr46/meiki.txt.recognition.v0/resolve/main/meiki.text.rec.v0.960x32.onnx" \
      models/meiki/meiki.text.rec.v0.960x32.onnx
fetch "$MHF/rtr46/meiki.txt.recognition.v0/resolve/main/meiki.text.rec.v0.vertical.32x480.onnx" \
      models/meiki/meiki.text.rec.v0.vertical.32x480.onnx

# PP-OCRv5 mobile det+rec, RapidOCR ONNX distribution (v3.9.2 model tag).
MS=https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.2
fetch "$MS/onnx/PP-OCRv5/det/ch_PP-OCRv5_det_mobile.onnx" models/ppocrv5/ch_PP-OCRv5_det_mobile.onnx
fetch "$MS/onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile.onnx" models/ppocrv5/ch_PP-OCRv5_rec_mobile.onnx
fetch "$MS/paddle/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile/ppocrv5_dict.txt" models/ppocrv5/ppocrv5_dict.txt

# manga-ocr ONNX export (mayocream) + tokenizer/config files.
MO=$MHF/mayocream/manga-ocr-onnx/resolve/main
fetch "$MO/encoder_model.onnx"       models/manga-ocr/encoder_model.onnx
fetch "$MO/decoder_model.onnx"       models/manga-ocr/decoder_model.onnx
fetch "$MO/vocab.txt"                models/manga-ocr/vocab.txt
fetch "$MO/config.json"              models/manga-ocr/config.json
fetch "$MO/generation_config.json"   models/manga-ocr/generation_config.json
fetch "$MO/preprocessor_config.json" models/manga-ocr/preprocessor_config.json
fetch "$MO/special_tokens_map.json"  models/manga-ocr/special_tokens_map.json
fetch "$MO/tokenizer_config.json"    models/manga-ocr/tokenizer_config.json

sort -u -k2 "$SUMS" -o "$SUMS"

# ------------------------------------------------------------------ provenance
{
    echo "date: $(date -Is)"
    echo "python: $(.venv/bin/python --version)"
    echo "tesseract: $(.mamba/envs/tess/bin/tesseract --version 2>&1 | head -1)"
    for c in "${OCR_BENCH_CHROMIUM:-}" chromium chromium-browser google-chrome-stable google-chrome chrome; do
        if [ -n "$c" ] && command -v "$c" >/dev/null 2>&1; then
            echo "chromium: $("$c" --version)"; break
        fi
    done
    echo "--- pip freeze"
    .venv/bin/pip freeze
} > results/env.txt

echo "setup complete"
