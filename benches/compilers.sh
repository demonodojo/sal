#!/usr/bin/env bash
# Compara el compilador bootstrap (Rust, release) con el escrito en sal
# (stage1: selfhost/main.sal compilado por el arranque con --release, clang -O3).
#
# ir — emitir IR. Los dos producen el texto que compara tests/selfhost_ir.rs.
#   rust:     sal emit ir ARCHIVO
#             parse + bajar + fusionar. Este subcomando no usa la caché incremental.
#   sal frío: stage1 ARCHIVO con SAL_SELFHOST_CACHE nuevo en cada repetición.
#   sal caché: la misma caché tras un calentamiento (acierto; no recompila).
#
# build — binario, los dos a -O0. No es el mismo pipeline; se imprime aparte.
#   rust: LLVM + clang -O0. La caché vive en target/incremental.
#         En acierto no regenera el .o del módulo; el enlace del runtime sí corre.
#   sal:  emit_c + clang -O0 (sal_clang). emit_c no consulta la caché de IR.
#
# Uso, desde cualquier sitio:
#   ./benches/compilers.sh
#   BENCH_RUNS=5 BENCH_LARGE_RUNS=2 BENCH_REBUILD=1 ./benches/compilers.sh
#   BENCH_BUILD_LARGE=1  incluye el enlace de selfhost/main.sal (lento).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
# El entorno del agente a veces apunta CARGO_TARGET_DIR a una caché externa.
export CARGO_TARGET_DIR="$ROOT/target"

RUNS="${BENCH_RUNS:-5}"
LARGE_RUNS="${BENCH_LARGE_RUNS:-2}"
RUST="$ROOT/target/release/sal"
STAGE1="$ROOT/target/bench/stage1"
OUT="$ROOT/target/bench"
INC="$ROOT/target/incremental"

SMALL_FILES=(
  examples/hello.sal
  examples/forward.sal
  corpus/forward_fused.sal
  examples/plot_csv.sal
)
LARGE_FILE="selfhost/main.sal"

if [[ ! -x "$RUST" || "${BENCH_REBUILD:-}" == 1 ]]; then
  echo "== cargo build --release" >&2
  cargo build --release
fi

if [[ ! -x "$STAGE1" || "${BENCH_REBUILD:-}" == 1 ]]; then
  echo "== stage1: sal build --release selfhost/main.sal" >&2
  mkdir -p "$OUT"
  bin="$("$RUST" build --release "$ROOT/selfhost/main.sal")"
  cp -f "$bin" "$STAGE1"
  chmod +x "$STAGE1"
fi

mkdir -p "$OUT"
TIMES="$(mktemp)"
trap 'rm -f "$TIMES"' EXIT

# Segundos (float) de un comando. stdout a /dev/null. Falla si el exit no es 0.
time_one() {
  local start end err
  err="$(mktemp)"
  start="$(date +%s.%N)"
  if ! "$@" > /dev/null 2>"$err"; then
    echo "falló: $*" >&2
    cat "$err" >&2
    rm -f "$err"
    return 1
  fi
  end="$(date +%s.%N)"
  rm -f "$err"
  awk -v s="$start" -v e="$end" 'BEGIN { printf "%.6f\n", e - s }'
}

# $1 archivo de tiempos (una línea por repetición). Imprime mediana min max en ms.
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

# stage1 con caché fría: un directorio nuevo por repetición.
repeat_sal_cold() {
  local n="$1"
  local file="$2"
  local cache
  cache="$(mktemp -d "$OUT/cache.XXXXXX")"
  env SAL_SELFHOST_CACHE="$cache" "$STAGE1" "$ROOT/$file" > /dev/null
  rm -rf "$cache"
  : > "$TIMES"
  local i
  for ((i = 0; i < n; i++)); do
    cache="$(mktemp -d "$OUT/cache.XXXXXX")"
    time_one env SAL_SELFHOST_CACHE="$cache" "$STAGE1" "$ROOT/$file" >> "$TIMES"
    rm -rf "$cache"
  done
  stats_ms "$TIMES"
}

repeat_sal_warm() {
  local n="$1"
  local file="$2"
  local cache="$OUT/warm-$(echo "$file" | tr '/' '-')"
  rm -rf "$cache"
  mkdir -p "$cache"
  env SAL_SELFHOST_CACHE="$cache" "$STAGE1" "$ROOT/$file" > /dev/null
  repeat "$n" env SAL_SELFHOST_CACHE="$cache" "$STAGE1" "$ROOT/$file"
}

lines_of() {
  wc -l < "$ROOT/$1" | tr -d ' '
}

# Tres tripletas "mediana min max" → una fila.
row() {
  local file="$1" rust_s="$2" cold_s="$3" warm_s="$4"
  python3 - "$file" "$(lines_of "$file")" "$rust_s" "$cold_s" "$warm_s" << 'PY'
import sys
file, lines, rust, cold, warm = sys.argv[1:]
def med(s):
    return float(s.split()[0])
rm, cm, wm = med(rust), med(cold), med(warm)
ratio = cm / rm if rm else float("inf")
print(f"{file:<28} {lines:>6} {rust.split()[0]:>10} {cold.split()[0]:>10} {warm.split()[0]:>10} {ratio:>8.2f}×")
print(f"{'':<28} {'':>6} {rust.split()[1]+'–'+rust.split()[2]:>10} {cold.split()[1]+'–'+cold.split()[2]:>10} {warm.split()[1]+'–'+warm.split()[2]:>10}")
PY
}

echo "rust  $RUST"
echo "sal   $STAGE1  (stage1, -O3)"
echo "repeticiones  pequeños=$RUNS  selfhost/main.sal=$LARGE_RUNS"
echo "tiempos en ms: mediana, y debajo el rango min–max."
echo "en ir, una pasada de calentamiento no entra en la mediana."
echo
printf "%-28s %6s %10s %10s %10s %9s\n" "programa" "lineas" "rust" "sal frio" "sal cache" "frio/rust"
printf "%-28s %6s %10s %10s %10s %9s\n" "--------" "------" "----" "--------" "---------" "---------"

bench_ir() {
  local file="$1" n="$2"
  echo "== ir $file" >&2
  # Calentamiento del binario rust (no entra en la mediana).
  "$RUST" emit ir "$ROOT/$file" > /dev/null
  local rust_s cold_s warm_s
  rust_s="$(repeat "$n" "$RUST" emit ir "$ROOT/$file")"
  cold_s="$(repeat_sal_cold "$n" "$file")"
  warm_s="$(repeat_sal_warm "$n" "$file")"
  row "$file" "$rust_s" "$cold_s" "$warm_s"
}

for f in "${SMALL_FILES[@]}"; do
  if ! env SAL_SELFHOST_CACHE="$OUT/probe-cache" "$STAGE1" "$ROOT/$f" > /dev/null 2>"$OUT/probe.err"; then
    echo "$f  (stage1 no lo acepta; se omite)"
    sed 's/^/  /' "$OUT/probe.err" | head -n 5
    continue
  fi
  bench_ir "$f" "$RUNS"
done

bench_ir "$LARGE_FILE" "$LARGE_RUNS"

echo
echo "build a binario (pipeline distinto, los dos a -O0: rust LLVM; sal C + clang)"
printf "%-28s %6s %10s %10s %10s\n" "programa" "lineas" "rust frio" "rust cache" "sal -O0"
printf "%-28s %6s %10s %10s %10s\n" "--------" "------" "---------" "----------" "-------"

# Guarda la caché incremental del usuario y la restaura al salir de este tramo.
INC_BAK=""
restore_inc() {
  if [[ -n "${INC_BAK:-}" && -d "$INC_BAK/saved" ]]; then
    rm -rf "$INC"
    mv "$INC_BAK/saved" "$INC"
  fi
  if [[ -n "${INC_BAK:-}" ]]; then
    rm -rf "$INC_BAK"
  fi
}
if [[ -d "$INC" ]]; then
  INC_BAK="$(mktemp -d "$OUT/incbak.XXXXXX")"
  mv "$INC" "$INC_BAK/saved"
fi
trap 'rm -f "$TIMES"; restore_inc' EXIT

bench_build() {
  local file="$1" n="$2"
  echo "== build $file" >&2
  local i
  : > "$TIMES"
  for ((i = 0; i < n; i++)); do
    rm -rf "$INC"
    time_one "$RUST" build "$ROOT/$file" >> "$TIMES"
  done
  local cold_s
  cold_s="$(stats_ms "$TIMES")"
  # La última compilación dejó la caché caliente.
  local warm_s
  warm_s="$(repeat "$n" "$RUST" build "$ROOT/$file")"
  : > "$TIMES"
  for ((i = 0; i < n; i++)); do
    time_one "$STAGE1" "$ROOT/$file" -o "$OUT/sal-out-$i" >> "$TIMES"
  done
  local sal_s
  sal_s="$(stats_ms "$TIMES")"
  python3 - "$file" "$(lines_of "$file")" "$cold_s" "$warm_s" "$sal_s" << 'PY'
import sys
file, lines, cold, warm, sal = sys.argv[1:]
print(f"{file:<28} {lines:>6} {cold.split()[0]:>10} {warm.split()[0]:>10} {sal.split()[0]:>10}")
PY
}

for f in examples/hello.sal examples/forward.sal corpus/forward_fused.sal; do
  bench_build "$f" "$RUNS"
done

if [[ "${BENCH_BUILD_LARGE:-}" == 1 ]]; then
  bench_build "$LARGE_FILE" "$LARGE_RUNS"
fi
