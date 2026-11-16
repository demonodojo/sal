#!/bin/sh
# Publica el paquete `sal` para soso (compilador nativo, backend LLVM y SDK) en
# paquetes/, que se sirve tal cual por raw.githubusercontent.com:
#
#   https://raw.githubusercontent.com/demonodojo/sal/main/paquetes
#
# `soso-pkg` lo tiene como origen por defecto, así que tras el push basta con
# `soso-pkg actualizar sal` en la máquina.
#
#   scripts/paquete-soso.sh [etiqueta]     por defecto v<versión de Cargo.toml>
#
# Compila en un worktree limpio de la etiqueta: lo publicado se puede rehacer
# desde un commit, nunca desde cambios sin commitear. La receta (SDK y
# toolchain para soso) vive en soso: SOSO apunta a su checkout (por defecto
# ../soso) y necesita su toolchain nativa (docs/SELF-HOSTING.md de soso).
# Una versión ya publicada no se rehace salvo con FORZAR=1.
set -eu
cd "$(dirname "$0")/.."
repo=$PWD
SOSO="${SOSO:-$repo/../soso}"
version=$(sed -n '/^\[package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)
etiqueta="${1:-v$version}"
if ! git rev-parse -q --verify "refs/tags/$etiqueta" >/dev/null; then
    echo "paquete-soso: no existe la etiqueta $etiqueta" >&2
    exit 1
fi
# Junto a soso, no dentro de Sal: hay versiones que dependen de
# ../soso/crates/sosomodel por ruta.
wt="$(cd "$SOSO/.." && pwd)/sal-paquete-$etiqueta"
git worktree remove --force "$wt" 2>/dev/null || rm -rf "$wt"
mkdir -p "$(dirname "$wt")"
git worktree add --detach "$wt" "$etiqueta" >/dev/null
trap 'git worktree remove --force "$wt" 2>/dev/null || true' EXIT
forzar=
[ "${FORZAR:-}" = 1 ] && forzar=--forzar
(cd "$SOSO" && cargo xtask sal-paquete --fuente "$wt" --out "$repo/paquetes" $forzar)
echo "paquete-soso: listo. Publica con: git add paquetes && git commit -m \"Paquete soso $etiqueta\" && git push"
