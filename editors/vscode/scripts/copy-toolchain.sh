#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
ext="$(cd "$(dirname "$0")/.." && pwd)"
arch="$(uname -m)"
case "$arch" in
  x86_64) triplet=linux-x64 ;;
  aarch64) triplet=linux-arm64 ;;
  *)
    echo "copy-toolchain: arquitectura no soportada: $arch" >&2
    exit 1
    ;;
esac
dest="$ext/bin/$triplet"
mkdir -p "$dest"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
cargo build --locked --release --manifest-path "$root/Cargo.toml" --bin sal --bin standard
cp "$CARGO_TARGET_DIR/release/sal" "$dest/sal"
cp "$CARGO_TARGET_DIR/release/standard" "$dest/standard"
chmod +x "$dest/sal" "$dest/standard"
"$dest/sal" --version
"$dest/standard" --version 2>/dev/null || true
echo "toolchain en $dest"
