#!/usr/bin/env bash
# Batería general: cada benches/suite/<carga>.sal frente a su <carga>.rs (rustc -O3).
# Mide tiempo de pared (mediana, con os.wait4) y RSS pico (/usr/bin/time).
#
# Uso, desde la raíz del repo:
#   ./benches/suite.sh                 # todas las cargas
#   ./benches/suite.sh tree sort       # solo esas
#   BENCH_RUNS=9 BENCH_REBUILD=1 ./benches/suite.sh
#   BENCH_LABEL=antes ./benches/suite.sh ; (cambio) ; BENCH_LABEL=despues ./benches/suite.sh
#   BENCH_COMPARE=antes,despues ./benches/suite.sh   # compara resultados guardados
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="$ROOT/target"

RUNS="${BENCH_RUNS:-5}"
LABEL="${BENCH_LABEL:-actual}"
BASE="$ROOT/target/bench/suite"
OUT="$BASE/$LABEL"

if [[ -n "${BENCH_COMPARE:-}" ]]; then
  python3 - "$BASE" "$BENCH_COMPARE" << 'PY'
import json
import sys

base, labels = sys.argv[1], sys.argv[2].split(",")
rows = [json.load(open(f"{base}/{l}/results.json")) for l in labels]
print(f"{'carga':<10}" + "".join(f" {l + ' ms':>12} {l + ' RSS':>11}" for l in labels))
for p in rows[0]:
    line = f"{p:<10}"
    for r in rows:
        x = r.get(p, {})
        line += f" {x.get('sal_ms', 0):12.1f} {x.get('sal_rss_kb', 0) / 1024:10.1f}M"
    print(line)
PY
  exit 0
fi

if [[ $# -gt 0 ]]; then
  PROGS=("$@")
else
  PROGS=()
  for f in benches/suite/*.sal; do
    PROGS+=("$(basename "$f" .sal)")
  done
fi

mkdir -p "$OUT"

if [[ ! -x "$ROOT/target/release/sal" || "${BENCH_REBUILD:-}" == 1 ]]; then
  echo "== cargo build --release (sal)" >&2
  cargo build --release
fi
SAL="$ROOT/target/release/sal"

for p in "${PROGS[@]}"; do
  if [[ ! -x "$BASE/$p-rust" || "benches/suite/$p.rs" -nt "$BASE/$p-rust" || "${BENCH_REBUILD:-}" == 1 ]]; then
    echo "== rustc -C opt-level=3 benches/suite/$p.rs" >&2
    rustc -C opt-level=3 "benches/suite/$p.rs" -o "$BASE/$p-rust"
  fi
  echo "== sal build --release benches/suite/$p.sal" >&2
  bin="$("$SAL" build --release "benches/suite/$p.sal")"
  cp -f "$bin" "$OUT/$p-sal"
done

python3 - "$RUNS" "$BASE" "$OUT" "${PROGS[@]}" << 'PY'
import json
import os
import statistics
import sys
import time

runs = int(sys.argv[1])
base, out = sys.argv[2], sys.argv[3]
progs = sys.argv[4:]


def run(binary):
    # fork/exec + wait4: tiempo exacto y RSS pico del hijo, sin los escalones de
    # subprocess.run(timeout=…).
    devnull = os.open(os.devnull, os.O_RDWR)
    t0 = time.perf_counter_ns()
    pid = os.fork()
    if pid == 0:
        os.dup2(devnull, 0)
        os.dup2(devnull, 1)
        os.dup2(devnull, 2)
        os.execv(binary, [binary])
    _, status, _ = os.wait4(pid, 0)
    dt = (time.perf_counter_ns() - t0) / 1e6
    os.close(devnull)
    return os.waitstatus_to_exitcode(status), dt


def peak_rss_kb(binary):
    # ru_maxrss de un hijo de Python arrastra el RSS del intérprete copiado por fork
    # (~8 MB); /usr/bin/time es un proceso pequeño y mide solo el programa.
    path = f"{out}/rss.txt"
    pid = os.fork()
    if pid == 0:
        devnull = os.open(os.devnull, os.O_RDWR)
        os.dup2(devnull, 0)
        os.dup2(devnull, 1)
        os.dup2(devnull, 2)
        os.execv("/usr/bin/time", ["/usr/bin/time", "-f", "%M", "-o", path, binary])
    os.wait4(pid, 0)
    vals = [int(l) for l in open(path) if l.strip().isdigit()]
    return vals[-1] if vals else 0


results = {}
print(f"repeticiones={runs}  tiempo = mediana en ms; memoria = RSS pico")
print()
print(f"{'carga':<10} {'rust':>9} {'sal':>9} {'sal/rust':>9} {'rust RSS':>10} {'sal RSS':>10} {'sal/rust':>9}")
for p in progs:
    per = {}
    for impl, binary in (("rust", f"{base}/{p}-rust"), ("sal", f"{out}/{p}-sal")):
        code, _ = run(binary)  # calentamiento
        ts = [run(binary)[1] for _ in range(runs)]
        per[impl] = (code, statistics.median(ts), peak_rss_kb(binary))
    if per["rust"][0] != per["sal"][0]:
        sys.exit(f"{p}: código de salida distinto: rust={per['rust'][0]} sal={per['sal'][0]}")
    r, s = per["rust"], per["sal"]
    results[p] = {"rust_ms": r[1], "sal_ms": s[1], "rust_rss_kb": r[2], "sal_rss_kb": s[2], "exit": s[0]}
    print(
        f"{p:<10} {r[1]:8.1f} {s[1]:9.1f} {s[1] / r[1]:8.2f}x"
        f" {r[2] / 1024:9.1f}M {s[2] / 1024:9.1f}M {s[2] / r[2]:8.2f}x"
    )
json.dump(results, open(f"{out}/results.json", "w"), indent=1)
print()
print("sal/rust >1 ⇒ sal usa más tiempo o más memoria.")
PY
