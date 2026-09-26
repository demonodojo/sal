---
name: sal-verificar
description: >-
  Verifica cambios de sal con cargo test y la CLI sal (check, fmt, emit, build,
  run). Usar al terminar un cambio del compilador, del runtime, de un programa
  .sal, de la extensión o del autohospedaje, antes de dar la tarea por hecha.
---

# Verificar un cambio

Comprobar el pase tocado, no la suite entera por costumbre. El binario de desarrollo es `cargo run --` o `target/debug/sal` después de `cargo build`.

## Por clase de cambio

| Se tocó | Comando |
|---------|---------|
| Lexer o parser | `cargo test --test frontend --test parse_and_fmt` |
| Tipos, ownership, efectos, lugares | `cargo test --test semantics` |
| IR o fusión | `cargo test --test forward_ir` |
| Runtime o kernels | `cargo test --test kernels --test exec_matmul --test exec_salt` |
| LLVM o enlace | `cargo test --test run_hello` |
| Instrumentación | `cargo test --test instrument_events --test instrument_leak --test instrument_sal` |
| CLI, módulos o caché | `cargo test --test toolchain --test run_hello` |
| Un `.sal` concreto | `cargo run -- check <fichero>` y, si debe ejecutar, `cargo run -- run <fichero>` |
| `selfhost/`, preludio o `corpus/` | `cargo test --test selfhost_ir` y la skill [sal-autohospedaje](../sal-autohospedaje/SKILL.md) |

Si el test nuevo afirma un diagnóstico, el código tiene que ser el de la lista cerrada (`E_PARSE`, `E_TYPE`, `E_MOVED`, `E_EFFECT`, `E_PLACE`, `E_SHAPE`, `E_TENSOR_ELEM`, `E_DEVICE`, `E_DEVICE_MISSING`, `E_OOM`).

## CLI

```bash
cargo run -- check examples/hello.sal
cargo run -- fmt examples/hello.sal
cargo run -- emit ast examples/hello.sal
cargo run -- emit ir examples/forward.sal
cargo run -- run examples/hello.sal
```

- `fmt` escribe la fuente canónica por stdout y sale 0 solo si parsea. Si no parsea, no imprime una reescritura.
- `emit ast` es el JSON que `sal compile --ast` vuelve a aceptar.
- `--error-format json` es una línea de stderr por diagnóstico, con `code`, `message`, `span` (`start`/`end` en bytes UTF-8; `line` y `col` desde 1) y `hint`.
- `--instrument` cambia el binario. Un programa limpio sale 0. Un evento (`LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN`, `INF`) sale distinto de 0.
- `sal check` acepta un programa de GPU o TPU aunque falte el dispositivo. `sal build --device gpu` o `tpu` falla con `E_DEVICE_MISSING` si no está el toolchain.

## Hecho

El test del pase pasa y, si el cambio es de superficie, `fmt` es idempotente sobre un ejemplo que use la construcción nueva.
