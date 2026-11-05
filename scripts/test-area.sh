#!/usr/bin/env bash
# Ejecuta el test del pase tocado, según la tabla «Verificar» de AGENTS.md.
# Uso: scripts/test-area.sh <área>      (sin área: lista las áreas)
set -euo pipefail

cd "$(dirname "$0")/.."

usage() {
    cat <<'EOF'
Áreas:
  frontend     lexer / parser          cargo test --test frontend --test parse_and_fmt
  semantics    tipos, ownership, efectos, lugares
  drops        drops
  ir           IR / fusión             cargo test --test forward_ir
  kernels      runtime / kernels
  llvm         LLVM / enlace           cargo test --test run_hello
  instrument   instrumentación
  toolchain    CLI / módulos / caché / paquetes
  lang         comportamiento del lenguaje (tests/lang)
  builtins     builtins mal usados
  target       destino linux/soso
  std          std/                    cargo run -- test
  standard     linter standard         (serie)
  selfhost     selfhost / preludio / corpus  (serie)
EOF
}

if [[ $# -lt 1 ]]; then
    usage
    exit 0
fi

case "$1" in
    frontend)   cargo test --test frontend --test parse_and_fmt ;;
    semantics)  cargo test --test semantics ;;
    drops)      cargo test --test drops ;;
    ir)         cargo test --test forward_ir ;;
    kernels)    cargo test --test kernels --test exec_matmul --test exec_column --test exec_salt ;;
    llvm)       cargo test --test run_hello ;;
    instrument) cargo test --test instrument_events --test instrument_leak --test instrument_sal ;;
    toolchain)  cargo test --test toolchain --test run_hello ;;
    lang)       cargo test --test lang ;;
    builtins)   cargo test --test builtin_errors ;;
    target)     cargo test --test soso_target --test cli_target --test std_io_linux ;;
    std)        cargo run -- test ;;
    standard)   cargo test --test standard -- --test-threads=1 ;;
    selfhost)   cargo test --test selfhost_ir -- --test-threads=1 ;;
    *)
        echo "área desconocida: $1" >&2
        usage >&2
        exit 2
        ;;
esac
