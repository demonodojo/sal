#!/usr/bin/env bash
# Cargas de cadenas (benches/strings/*.sal) frente al mismo programa en Rust (-O3):
# tiempo, RSS pico y, con valgrind (dhat), número de reservas y bytes reservados.
#
# Los binarios de sal se guardan en target/bench/strings/<etiqueta>/, así que se puede
# medir antes y después de un cambio del compilador o del runtime y compararlos:
#   BENCH_LABEL=antes   ./benches/strings.sh
#   (cambio)
#   BENCH_LABEL=despues ./benches/strings.sh
#   BENCH_COMPARE=antes,despues ./benches/strings.sh   # solo compara, no recompila
#
# Variables: BENCH_RUNS (5), BENCH_DHAT=0 para saltar valgrind, BENCH_REBUILD=1.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export CARGO_TARGET_DIR="$ROOT/target"

RUNS="${BENCH_RUNS:-5}"
LABEL="${BENCH_LABEL:-actual}"
BASE="$ROOT/target/bench/strings"
PROGS=(concat2 concat_many accumulate accumulate_multi large interp clone slice)

if [[ -n "${BENCH_COMPARE:-}" ]]; then
  python3 - "$BASE" "${BENCH_COMPARE}" << 'PY'
import json
import sys

base, labels = sys.argv[1], sys.argv[2].split(",")
rows = [json.load(open(f"{base}/{l}/results.json")) for l in labels]
print(f"{'carga':<17}" + "".join(f" {l + ' ms':>12} {l + ' allocs':>14} {l + ' MB':>10}" for l in labels))
for p in rows[0]:
    line = f"{p:<17}"
    for r in rows:
        x = r.get(p, {})
        allocs = x.get("blocks")
        mb = x.get("bytes")
        line += f" {x.get('sal_ms', 0):12.1f} {allocs if allocs is not None else '-':>14}"
        line += f" {mb / 1e6 if mb is not None else 0:10.1f}"
    print(line)
PY
  exit 0
fi

OUT="$BASE/$LABEL"
mkdir -p "$OUT"

if [[ ! -x "$ROOT/target/release/sal" || "${BENCH_REBUILD:-}" == 1 ]]; then
  echo "== cargo build --release (sal)" >&2
  cargo build --release
fi
SAL="$ROOT/target/release/sal"

for p in "${PROGS[@]}"; do
  if [[ ! -x "$BASE/$p-rust" || "benches/strings/$p.rs" -nt "$BASE/$p-rust" ]]; then
    echo "== rustc -C opt-level=3 benches/strings/$p.rs" >&2
    rustc -C opt-level=3 "benches/strings/$p.rs" -o "$BASE/$p-rust"
  fi
  echo "== sal build --release benches/strings/$p.sal" >&2
  bin="$("$SAL" build --release "benches/strings/$p.sal")"
  cp -f "$bin" "$OUT/$p-sal"
done

echo "== medición ($LABEL)" >&2
python3 - "$RUNS" "$BASE" "$OUT" "${BENCH_DHAT:-1}" "${PROGS[@]}" << 'PY'
import json
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

runs, base, out, dhat = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4] == "1"
progs = sys.argv[5:]
dhat = dhat and shutil.which("valgrind") is not None


def run(binary):
    # El resultado va en el código de salida, no en 0.
    return subprocess.run([binary], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL, timeout=600)


def peak_rss_kb(binary):
    with tempfile.NamedTemporaryFile(mode="w+") as f:
        subprocess.run(["/usr/bin/time", "-f", "%M", "-o", f.name, binary],
                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                       stderr=subprocess.DEVNULL)
        f.seek(0)
        vals = [int(l.strip()) for l in f if l.strip().isdigit()]
    return vals[-1] if vals else 0


def median_ms(binary):
    run(binary)  # calentamiento
    xs = []
    for _ in range(runs):
        t0 = time.monotonic()
        run(binary)
        xs.append(time.monotonic() - t0)
    return statistics.median(xs) * 1000


def dhat_totals(binary):
    """Reservas y bytes reservados en todo el proceso (valgrind --tool=dhat)."""
    with tempfile.NamedTemporaryFile(suffix=".json") as f:
        r = subprocess.run(["valgrind", "--tool=dhat", f"--dhat-out-file={f.name}", binary],
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                           stderr=subprocess.PIPE, text=True, timeout=1800)
    m = re.search(r"Total:\s+([\d,]+) bytes in ([\d,]+) blocks", r.stderr)
    if not m:
        return None, None
    return int(m.group(1).replace(",", "")), int(m.group(2).replace(",", ""))


results = {}
for p in progs:
    rust, sal = f"{base}/{p}-rust", f"{out}/{p}-sal"
    rc, sc = run(rust).returncode, run(sal).returncode
    if rc != sc:
        sys.exit(f"{p}: código de salida distinto: rust={rc} sal={sc}")
    row = {"exit": sc, "rust_ms": median_ms(rust), "sal_ms": median_ms(sal),
           "rust_rss_kb": peak_rss_kb(rust), "sal_rss_kb": peak_rss_kb(sal)}
    if dhat:
        row["bytes"], row["blocks"] = dhat_totals(sal)
    results[p] = row

json.dump(results, open(f"{out}/results.json", "w"), indent=1)
print(f"repeticiones={runs}; tiempo = mediana en ms; RSS pico; reservas y bytes de sal según dhat")
print(f"{'carga':<17} {'rust ms':>9} {'sal ms':>9} {'sal/rust':>9} {'sal RSS':>9} {'reservas':>12} {'MB res.':>9}")
for p, r in results.items():
    blocks = r.get("blocks")
    mb = r.get("bytes")
    print(f"{p:<17} {r['rust_ms']:9.1f} {r['sal_ms']:9.1f} {r['sal_ms'] / r['rust_ms']:8.2f}x"
          f" {r['sal_rss_kb'] / 1024:8.1f}M {blocks if blocks is not None else '-':>12}"
          f" {mb / 1e6 if mb is not None else 0:9.1f}")
PY
