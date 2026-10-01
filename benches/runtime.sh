#!/usr/bin/env bash
# Compara tiempo de ejecución: binario nativo Rust (-O3) vs programa sal compilado (--release).
#
# Uso, desde la raíz del repo:
#   ./benches/runtime.sh
#   BENCH_RUNS=7 BENCH_REBUILD=1 ./benches/runtime.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="$ROOT/target"

RUNS="${BENCH_RUNS:-5}"
SAL_SRC="$ROOT/benches/speed/work.sal"
RUST_SRC="$ROOT/benches/speed/work.rs"
OUT="$ROOT/target/bench/runtime"
RUST_BIN="$OUT/work-rust"
SAL_BIN=""

mkdir -p "$OUT"

if [[ ! -x "$ROOT/target/release/sal" || "${BENCH_REBUILD:-}" == 1 ]]; then
  echo "== cargo build --release (sal)" >&2
  cargo build --release
fi
SAL="$ROOT/target/release/sal"

if [[ ! -x "$RUST_BIN" || "${BENCH_REBUILD:-}" == 1 ]]; then
  echo "== rustc -C opt-level=3 benches/speed/work.rs" >&2
  rustc -C opt-level=3 "$RUST_SRC" -o "$RUST_BIN"
fi

echo "== sal build --release benches/speed/work.sal" >&2
SAL_BIN="$("$SAL" build --release "$SAL_SRC")"

TIMES="$(mktemp)"
trap 'rm -f "$TIMES"' EXIT

time_one() {
  local start end err
  err="$(mktemp)"
  start="$(date +%s.%N)"
  # El benchmark devuelve el resultado en el código de salida (p. ej. 202), no en 0.
  "$@" > /dev/null 2>"$err" || true
  end="$(date +%s.%N)"
  rm -f "$err"
  awk -v s="$start" -v e="$end" 'BEGIN { printf "%.6f\n", e - s }'
}

stats_ms() {
  python3 - "$1" << 'PY'
import sys
xs = sorted(float(l) for l in open(sys.argv[1]) if l.strip())
n = len(xs)
if n == 0:
    sys.exit("sin muestras")
med = xs[n // 2] if n % 2 else (xs[n // 2 - 1] + xs[n // 2]) / 2
print(f"{med * 1000:.1f} {min(xs) * 1000:.1f} {max(xs) * 1000:.1f}")
PY
}

repeat() {
  local n="$1"
  shift
  : > "$TIMES"
  local i
  for ((i = 0; i < n; i++)); do
    time_one "$@" >> "$TIMES"
  done
  stats_ms "$TIMES"
}

run_code() {
  set +e
  "$1" > /dev/null
  local ec=$?
  set -e
  printf '%s' "$ec"
}

RUST_CODE="$(run_code "$RUST_BIN")"
SAL_CODE="$(run_code "$SAL_BIN")"
if [[ "$RUST_CODE" != "$SAL_CODE" ]]; then
  echo "código de salida distinto (¿OUTER/INNER desincronizados entre .rs y .sal?):" >&2
  echo "rust: $RUST_CODE  sal: $SAL_CODE" >&2
  exit 1
fi
RESULT="$RUST_CODE"

echo "== calentamiento" >&2
set +e
"$RUST_BIN" > /dev/null
"$SAL_BIN" > /dev/null
set -e

rust_s="$(repeat "$RUNS" "$RUST_BIN")"
sal_s="$(repeat "$RUNS" "$SAL_BIN")"

python3 - "$RESULT" "$RUNS" "$rust_s" "$sal_s" << 'PY'
import sys
result, runs, rust, sal = sys.argv[1:]
def med(s):
    return float(s.split()[0])
rm, sm = med(rust), med(sal)
ratio = sm / rm if rm else float("inf")
print(f"work(6000×6000)  exit={result.strip()} (byte bajo del Int; ver test bench_runtime)")
print(f"repeticiones={runs}  tiempos en ms (mediana; debajo min–max)")
print()
print(f"{'':12} {'mediana':>10}  min–max")
print(f"{'rust opt3':12} {rust.split()[0]:>10}  {rust.split()[1]}–{rust.split()[2]}")
print(f"{'sal release':12} {sal.split()[0]:>10}  {sal.split()[1]}–{sal.split()[2]}")
print()
print(f"sal/rust ≈ {ratio:.2f}×  (>1 ⇒ sal más lento)")
PY
