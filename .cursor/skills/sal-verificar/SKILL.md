---
name: sal-verificar
description: >-
  Verifica cambios de sal con cargo test y la CLI sal (check, fmt, emit, build,
  run) y el linter standard (check, fix, y reglas nuevas cuando haga falta).
  Usar al terminar un cambio del compilador, del runtime, de un programa .sal,
  de standard/, de la extensión o del autohospedaje, antes de dar la tarea por
  hecha.
---

# Verificar un cambio

Comprobar el pase tocado, no la suite entera por costumbre. El binario de desarrollo es `cargo run --` o `target/debug/sal` después de `cargo build`.

## Por clase de cambio

| Se tocó | Comando |
|---------|---------|
| Lexer o parser | `cargo test --test frontend --test parse_and_fmt` |
| Tipos, ownership, efectos, lugares (`frame`, ops columnares, …) | `cargo test --test semantics` |
| IR o fusión (`ColumnBin`, `TensorBin`, …) | `cargo test --test forward_ir` |
| Runtime o kernels (matmul, **tensor bin**, …) | `cargo test --test kernels --test exec_matmul --test exec_column --test exec_salt` |
| LLVM o enlace | `cargo test --test run_hello` |
| Instrumentación | `cargo test --test instrument_events --test instrument_leak --test instrument_sal` |
| CLI, módulos o caché | `cargo test --test toolchain --test run_hello` |
| Paquetes git, `Sal.lock`, `sal_modules/` | `cargo test --test toolchain fetch_ sal_modules_import import_not install_` y `cargo test --test selfhost_ir selfhost_resolves_git` |
| Comportamiento del lenguaje (programa, salida, error) | `cargo test --test lang` y, si el selfhost lo implementa, `cargo test --test selfhost_ir selfhost_meets_language_conformance`; ver [sal-conformidad](../sal-conformidad/SKILL.md) |
| Atributos (`@frozen`, `@stack`, `@no_heap`, `@layout`) | `cargo test --test lang attr_` |
| Builtins mal usados | `cargo test --test builtin_errors` |
| Destino linux/soso, `syscall`, `std/io` | `cargo test --test soso_target --test cli_target --test std_io_linux` |
| `std/` (http, json, text, io) | `cargo run -- test` (ejecuta `tests/sal/test_*.sal`); ver [sal-stdlib](../sal-stdlib/SKILL.md) |
| Binarios auxiliares `som2salt` / `standard` | `cargo test --test bins --test som_export_errors` |
| Un `.sal` concreto | `cargo run -- check <fichero>` y, si debe ejecutar, `cargo run -- run <fichero>` |
| Estilo en `.sal` (no sintaxis) | `cargo run --bin standard -- check <fichero>`; opcional `fix` in situ. Ver [sal-standard](../sal-standard/SKILL.md) |
| `standard/` o cops de estilo | `cargo test --test standard -- --test-threads=1` |
| `selfhost/`, preludio o `corpus/` | `cargo test --test selfhost_ir` y la skill [sal-autohospedaje](../sal-autohospedaje/SKILL.md) |

Atajo: `scripts/test-area.sh <área>` ejecuta la fila correspondiente de esta tabla (`frontend`, `semantics`, `drops`, `ir`, `kernels`, `llvm`, `instrument`, `toolchain`, `lang`, `builtins`, `target`, `std`, `standard`, `selfhost`); sin argumento lista las áreas. Las áreas `standard` y `selfhost` van en serie (`--test-threads=1`).

Si el test nuevo afirma un diagnóstico, el código tiene que ser el de la lista cerrada (`E_PARSE`, `E_TYPE`, `E_MOVED`, `E_EFFECT`, `E_PLACE`, `E_SHAPE`, `E_TENSOR_ELEM`, `E_DEVICE`, `E_DEVICE_MISSING`, `E_OOM`; `E_TARGET` solo para destino).

`cargo test` completo compila y ejecuta muchos programas y puede tardar; `selfhost_ir` compila el selfhost varias veces. Para un cambio acotado, ejecuta primero el test del pase y deja la suite entera para el cierre o el release.

## CLI

```bash
cargo run -- check examples/hello.sal
cargo run -- fmt examples/hello.sal
cargo run -- emit ast examples/hello.sal
cargo run -- emit ir examples/forward.sal
cargo run -- emit ir corpus/column_ops.sal
cargo run -- run examples/hello.sal
cargo run -- fetch
cargo run -- fetch --update
cargo run -- install
cargo run -- test                       # tests/sal/test_*.sal
cargo run -- selfhost                   # stage1 y stage2
cargo run -- parity corpus/column_ops.sal          # IR arranque vs selfhost-stage2
cargo run -- parity x.sal --selfhost <bin>         # otro binario selfhost
cargo run -- --target soso check x.sal  # destino soso (kernel)
cargo run -- add mylib --git https://github.com/org/mylib --version 0.1
cargo run --bin standard -- check examples/hello.sal
cargo run --bin standard -- fix path/to/file.sal
```

- `fmt` escribe la fuente canónica por stdout y sale 0 solo si parsea. Si no parsea, no imprime una reescritura.
- `standard check` avisa de estilo (`Style/*`); `standard fix` parchea el fichero. No sustituye a `sal check` ni a `sal fmt`.
- `emit ast` es el JSON que `sal compile --ast` vuelve a aceptar.
- `--error-format json` es una línea de stderr por diagnóstico, con `code`, `message`, `span` (`start`/`end` en bytes UTF-8; `line` y `col` desde 1) y `hint`.
- `--instrument` cambia el binario. Un evento (`LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN`, `INF`) se imprime en stderr como JSON (`{"code":"LEAK","site":"…"}`) y el proceso sale con 2. Pero el código de salida también es el resultado del programa (`main` que devuelve 2 sale 2), así que **no** uses el código para detectar fugas: mira stderr.
- Comprobar fugas de un programa: `sal build --instrument f.sal -o f.bin && SAL_INSTRUMENT_STRICT=1 ./f.bin 2>&1 >/dev/null | grep -o 'LEAK[^}]*'`. Sin salida, limpio. `SAL_INSTRUMENT_STRICT=1` reporta también los sitios que el modo normal deja pasar (cadenas y cajas de structs del AST del selfhost).
- `sal check` acepta un programa de GPU o TPU aunque falte el dispositivo. `sal build --device gpu` o `tpu` falla con `E_DEVICE_MISSING` si no está el toolchain.
- Tras añadir o cambiar dependencias git en `Sal.toml`, ejecutar `sal fetch` desde la raíz del proyecto (donde está el manifiesto). Los tests de paquetes usan repos `file://` y fijan `SAL_PACKAGES` en un tempdir; no dependen de red.
- `--target linux|soso` (global). `syscall` solo existe en esos dos; en otro destino es `E_TARGET`.
- Proyectos fuera del árbol del compilador: los módulos importados están en `sal_modules/` de ese proyecto (`import "io/io.sal"` → `sal_modules/io/io.sal`).

## Hecho

El test del pase pasa y cubre lo añadido al lenguaje: el caso nuevo se ejecuta y el resultado es el que la SPEC fija. Si el cambio es de superficie, `fmt` es idempotente sobre un ejemplo que use la construcción nueva. Si los dos compiladores la implementan, pasan el test del arranque y `cargo test --test selfhost_ir` cuando el espejo en `selfhost/` puede cambiar la IR; un test solo del bootstrap no cierra el cambio.

Todo `.sal` editado pasa por `standard check`. Las prácticas ya cubiertas se corrigen con `standard fix` y se revisa el diff; `check` queda limpio en esos ficheros. Si el diff introduce una práctica que va a repetirse y ninguna regla la cubre, el cambio le añade esa capacidad a `standard` (cop, fixtures y test en `tests/standard.rs`). Ver [sal-standard](../sal-standard/SKILL.md).

Superficie `if` / `elsif` / `else`: `tests/frontend.rs` (`if_without_else_*`, `if_else_binds_to_inner_if`, `elsif_chain_is_flat`, `fmt_if_without_else_is_idempotent`, `fmt_elsif_is_idempotent`) y `tests/semantics.rs` (`if_without_else_as_stmt_ok_for_int_return`, `if_without_else_as_tail_rejects_non_unit_return`, `elsif_without_else_is_unit`, `elsif_with_else_has_then_type`, `elsif_without_else_as_stmt_ok`). Tras tocar `parse_if` en Rust o en `selfhost/`, incluir `cargo test --test selfhost_ir` si puede afectar la IR del corpus.
