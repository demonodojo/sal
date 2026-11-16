#!/usr/bin/env bash
# Compare sal tiny infer (prefill + decode) vs llama.cpp on the same tiny weights.
#
# Prerequisites: git, cmake, clang and python3. llama.cpp is cloned into target/ when
# it is not already built; tiny weights are generated synthetically unless MODEL_DIR
# points at a real sosomodel checkout.
#
# Usage (repo root):
#   ./benches/infer_llama.sh
#   BENCH_RUNS=7 LLAMA_SRC=../llama.cpp MODEL_DIR=/path/to/model ./benches/infer_llama.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"

RUNS="${BENCH_RUNS:-5}"
LLAMA_CPP="${LLAMA_CPP:-$ROOT/target/llama-cpu-build}"
LLAMA_SRC="${LLAMA_SRC:-$ROOT/target/llama.cpp}"
GGUF_MODEL="${GGUF_MODEL:-$ROOT/target/infer_salt/tiny/tiny-f32.gguf}"
GGUF_PY="${GGUF_PY:-$ROOT/target/gguf-venv/bin/python}"
BIN_DIR="$CARGO_TARGET_DIR/release"
SAL="${SAL:-$BIN_DIR/sal}"
MODEL_DIR="${MODEL_DIR:-${SAL_TINY_MODEL:-}}"
OUT="$ROOT/target/bench/infer"
mkdir -p "$OUT"

echo "== cargo build --release (sal + som2salt)" >&2
cargo build --release --bin sal --bin som2salt
SAL="$BIN_DIR/sal"

if [[ -n "$MODEL_DIR" ]]; then
  echo "== som2salt (model: $MODEL_DIR)" >&2
  "$BIN_DIR/som2salt" --model "$MODEL_DIR" >&2
else
  echo "== som2salt (synthetic tiny weights)" >&2
  "$BIN_DIR/som2salt" --synthetic >&2
fi

if [[ ! -x "$GGUF_PY" ]]; then
  echo "== python venv (gguf)" >&2
  python3 -m venv "$ROOT/target/gguf-venv"
  "$ROOT/target/gguf-venv/bin/pip" install -q gguf numpy
  GGUF_PY="$ROOT/target/gguf-venv/bin/python"
fi
echo "== som2gguf" >&2
"$GGUF_PY" "$ROOT/scripts/som2gguf.py" --salt-dir "$ROOT/target/infer_salt/tiny" --out "$GGUF_MODEL" >&2

time_median() {
  local bin="$1"
  local i t
  local -a samples=()
  for ((i = 0; i < RUNS; i++)); do
    t="$(/usr/bin/time -f '%e' "$bin" 2>&1 >/dev/null || true)"
    samples+=("$t")
  done
  printf '%s\n' "${samples[@]}" | sort -n | awk 'NR==int((NF+1)/2){print; exit}'
}

echo "== sal build --release examples/infer/tiny_prefill.sal" >&2
PREFILL_BIN="$("$SAL" build --release examples/infer/tiny_prefill.sal)"

DECODE_BIN=""
if "$SAL" check examples/infer/tiny_decode.sal >/dev/null 2>&1; then
  echo "== sal build --release examples/infer/tiny_decode.sal" >&2
  if DECODE_BIN="$("$SAL" build --release examples/infer/tiny_decode.sal 2>/dev/null)"; then
    :
  else
    echo "== tiny_decode build failed (LLVM/runtime; prefill still runs)" >&2
    DECODE_BIN=""
  fi
else
  echo "== skip tiny_decode (check failed)" >&2
fi

echo ""
echo "=== sal (CPU, release) ==="
if "$PREFILL_BIN" >/dev/null 2>&1; then
  PREFILL_S="$(time_median "$PREFILL_BIN")"
  echo "tiny_prefill: ${PREFILL_S}s (median of $RUNS runs)"
else
  echo "tiny_prefill: FAILED (segfault or missing salts — run from repo root; see LLVM/runtime gaps for rope/attention)"
  PREFILL_S=""
fi

if [[ -n "$DECODE_BIN" ]] && "$DECODE_BIN" >/dev/null 2>&1; then
  DECODE_S="$(time_median "$DECODE_BIN")"
  echo "tiny_decode:  ${DECODE_S}s (median of $RUNS runs, prefill+4 decode steps)"
else
  echo "tiny_decode:  FAILED (same as above)"
  DECODE_S=""
fi

LLAMA_BENCH="$LLAMA_CPP/bin/llama-bench"
if [[ ! -x "$LLAMA_BENCH" && -x "$LLAMA_SRC/build/bin/llama-bench" ]]; then
  LLAMA_BENCH="$LLAMA_SRC/build/bin/llama-bench"
  export LD_LIBRARY_PATH="${LLAMA_SRC}/build/bin:${LLAMA_SRC}/build/src:${LD_LIBRARY_PATH:-}"
fi
if [[ ! -x "$LLAMA_BENCH" ]]; then
  echo ""
  echo "=== llama.cpp ===" >&2
  if [[ ! -d "$LLAMA_SRC" ]]; then
    echo "llama.cpp not found; cloning into $LLAMA_SRC" >&2
    git clone --depth 1 https://github.com/ggml-org/llama.cpp.git "$LLAMA_SRC"
  fi
  echo "llama-bench not found; building CPU target under target/llama-cpu-build" >&2
  cmake -S "$LLAMA_SRC" -B "$ROOT/target/llama-cpu-build" -DGGML_CUDA=OFF -DBUILD_SHARED_LIBS=OFF -DLLAMA_BUILD_TESTS=OFF
  cmake --build "$ROOT/target/llama-cpu-build" -j"$(nproc)" --target llama-bench
  LLAMA_BENCH="$ROOT/target/llama-cpu-build/bin/llama-bench"
fi
if [[ ! -x "$LLAMA_BENCH" ]]; then
  echo "llama-bench missing after build"
  exit 1
fi

if [[ ! -f "$GGUF_MODEL" ]]; then
  echo ""
  echo "=== llama.cpp ==="
  echo "GGUF missing: $GGUF_MODEL"
  echo "Export F32 tiny to GGUF (same tensors as som2salt) and set GGUF_MODEL=..."
  exit 0
fi

echo ""
echo "=== llama.cpp (llama-bench) ==="
# Match tiny manifest: 4 layers, 128 dim, 4 heads, prefill 8 + 4 decode tokens
"$LLAMA_BENCH" -m "$GGUF_MODEL" -p 8 -n 4 -t "$(nproc)" 2>&1 | tee "$OUT/llama_bench.txt"

echo ""
echo "Compare sal printed lines (sal_bench ...) with llama-bench pp/tg ns/token when sal runs cleanly."
