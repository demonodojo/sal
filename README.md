# sal

Lenguaje de sistemas con ownership inferido, CPU/GPU/TPU en el tipo, y compilador bootstrap en Rust.

## Requisitos

- Rust 1.70+
- Clang (LLVM IR → binario)

## Instalación

```bash
cargo build --release
```

El binario queda en `target/release/sal`.

## Uso

```bash
sal check examples/hello.sal
sal build examples/hello.sal
sal run examples/hello.sal
sal fmt examples/hello.sal
sal emit ast examples/hello.sal
sal emit ir examples/forward.sal
```

Flags: `--release`, `--device cpu|gpu|tpu`, `--instrument`, `--instrument-out FILE`.

## Pipeline para agentes

```text
fuente ↔ AST (JSON) ↔ AST tipado ↔ IR ↔ LLVM IR
```

`sal emit ast` / `sal compile --ast` (JSON versionado en el AST).

## Proyecto

- `src/` — compilador bootstrap (Rust)
- `runtime/` — runtime C (kernels, instrumentación)
- `std/prelude.sal` — preludio
- `selfhost/` — compilador en sal (validación)
- `examples/` — ejemplos
- `corpus/` — programas para autohospedaje

Ver [SPEC.md](SPEC.md).
