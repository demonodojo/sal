#!/usr/bin/env bash
# Compara tiempo y memoria pico: binario nativo Rust (-O3) vs el mismo programa en sal
# compilado con --release. Las tres cargas asignan de verdad en el heap: lista de structs
# con String (node), lista de String con clone profundo (strings) y lista de Int (ints).
#
# Uso, desde la raíz del repo:
#   ./benches/memory.sh
#   BENCH_RUNS=9 BENCH_REBUILD=1 ./benches/memory.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="$ROOT/target"

RUNS="${BENCH_RUNS:-5}"
OUT="$ROOT/target/bench/memory"
PROGS=(node strings ints)

mkdir -p "$OUT"

if [[ ! -x "$ROOT/target/release/sal" || "${BENCH_REBUILD:-}" == 1 ]]; then
  echo "== cargo build --release (sal)" >&2
  cargo build --release
fi
SAL="$ROOT/target/release/sal"

for p in "${PROGS[@]}"; do
  if [[ ! -x "$OUT/$p-rust" || "benches/mem/$p.rs" -nt "$OUT/$p-rust" || "${BENCH_REBUILD:-}" == 1 ]]; then
    echo "== rustc -C opt-level=3 benches/mem/$p.rs" >&2
    rustc -C opt-level=3 "benches/mem/$p.rs" -o "$OUT/$p-rust"
  fi
  echo "== sal build --release benches/mem/$p.sal" >&2
  bin="$("$SAL" build --release "benches/mem/$p.sal")"
  cp -f "$bin" "$OUT/$p-sal"
done

echo "== calentamiento + medición" >&2
python3 - "$RUNS" "$OUT" "${PROGS[@]}" << 'PY'
import statistics
import subprocess
import sys
import tempfile
import time

runs = int(sys.argv[1])
out = sys.argv[2]
progs = sys.argv[3:]


def run(binary):
    # El benchmark devuelve el resultado en el código de salida (p. ej. 224), no en 0.
    return subprocess.run(
        [binary],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=600,
    )


def peak_rss_kb(binary):
    with tempfile.NamedTemporaryFile(mode="w+") as f:
        subprocess.run(
            ["/usr/bin/time", "-f", "%M", "-o", f.name, binary],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        f.seek(0)
        vals = [int(l.strip()) for l in f if l.strip().isdigit()]
    return vals[-1] if vals else 0


def wall_times(binary):
    xs = []
    for _ in range(runs):
        t0 = time.monotonic()
        run(binary)
        xs.append(time.monotonic() - t0)
    return xs


rows = {}
for p in progs:
    per = {}
    for impl in ("rust", "sal"):
        binary = f"{out}/{p}-{impl}"
        code = run(binary).returncode
        run(binary)  # calentamiento, fuera de la mediana
        ts = wall_times(binary)
        per[impl] = (code, statistics.median(ts), min(ts), max(ts), peak_rss_kb(binary))
    rc, sc = per["rust"][0], per["sal"][0]
    if rc != sc:
        sys.exit(
            f"{p}: código de salida distinto (¿N desincronizado entre .rs y .sal?): "
            f"rust={rc} sal={sc}"
        )
    rows[p] = per

print(f"repeticiones={runs}  tiempo = mediana en ms; memoria = RSS pico del proceso")
print()
print(
    f"{'carga':<10} {'rust':>9} {'sal':>9} {'sal/rust':>9}"
    f" {'rust RSS':>10} {'sal RSS':>10} {'sal/rust':>9}"
)
print(
    f"{'-' * 10:<10} {'-' * 9:>9} {'-' * 9:>9} {'-' * 9:>9}"
    f" {'-' * 10:>10} {'-' * 10:>10} {'-' * 9:>9}"
)
for p in progs:
    per = rows[p]
    rust_ms, sal_ms = per["rust"][1] * 1000, per["sal"][1] * 1000
    rust_rss, sal_rss = per["rust"][4] / 1024, per["sal"][4] / 1024
    print(
        f"{p + '.sal':<10} {rust_ms:8.1f} {sal_ms:8.1f} {sal_ms / rust_ms:8.2f}x"
        f" {rust_rss:9.1f}M {sal_rss:9.1f}M {sal_rss / rust_rss:8.2f}x"
    )
    print(
        f"{'':<10} min–max rust {per['rust'][2] * 1000:.1f}–{per['rust'][3] * 1000:.1f}"
        f"  sal {per['sal'][2] * 1000:.1f}–{per['sal'][3] * 1000:.1f}  exit={per['sal'][0]}"
    )
print()
print("sal/rust >1 ⇒ sal usa más tiempo o más memoria. Las tres cargas asignan en el heap")
print("(List[struct] con String, List[String] con clone profundo y List[Int]).")
PY
