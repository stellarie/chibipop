# Source (or let the scripts source) this before anything else.
# Pins every cache and download inside tools/ocr-bench/ so nothing lands in $HOME.
_OB_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export HF_HOME="$_OB_ROOT/models/hf"
export HUGGINGFACE_HUB_CACHE="$_OB_ROOT/models/hf/hub"
export TRANSFORMERS_CACHE="$_OB_ROOT/models/hf/transformers"
export MODELSCOPE_CACHE="$_OB_ROOT/.cache/modelscope"
export PIP_CACHE_DIR="$_OB_ROOT/.cache/pip"
export XDG_CACHE_HOME="$_OB_ROOT/.cache/xdg"
export MAMBA_ROOT_PREFIX="$_OB_ROOT/.mamba"
export TESSDATA_PREFIX="$_OB_ROOT/models/tessdata"
export OCR_BENCH_ROOT="$_OB_ROOT"
