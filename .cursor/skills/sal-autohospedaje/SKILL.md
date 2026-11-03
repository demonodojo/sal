---
name: sal-autohospedaje
description: >-
  Mantiene el compilador escrito en sal (selfhost/), el preludio y el corpus
  de las tres cadenas con IR idéntica. Usar al editar selfhost/ (lexer, parser,
  infer, ownership, effects, device, attrs, fuse, tensorops, emit, qualify,
  modules), std/prelude.sal, corpus/, o tests/selfhost_ir.rs, o al hablar de
  stage1, stage2, paridad con el arranque o autohospedaje.
---

# Autohospedaje

`selfhost/` es el mismo lenguaje, no un dialecto ni un programa que devuelve 0. Lee fuente, construye el AST de la gramática recursiva por la izquierda, tipa, posee, baja a la misma IR y emite código.

## Estado (no está completo)

La fuente de verdad del estado es la tabla «Estado» de [docs/subplans/07-selfhost.md](../../../docs/subplans/07-selfhost.md); léela antes de afirmar qué cubre el selfhost. Resumen a fecha de esta skill:

- **Hay** lexer, parser (incl. `match`, `enum`, `frame`, lambda, `try`, atributos), efectos, dispositivos, ownership, inferencia parcial, IR (`fuse.sal`, `tensorops.sal`, `tshape.sal`) y calificadores/módulos.
- **El backend (`emit.sal`) emite IR de texto y C**, no LLVM ni objeto propio. Dos caminos a C: desde el AST (escalares, strings, structs, bytes, `syscall`) y, para funciones con tensores, desde la IR (`cir_*`). Los buffers de tensor llevan longitud en cabecera de 16 bytes y no se liberan.
- **`infer.sal` modela escalares y parte de los tensores** (`matmul`, `transpose`, `reshape`, `softmax`, `relu`/`silu`/`rope`). `attention`, `qmatmul`, `reduce`, `rms_norm`, genéricos y builtins sin firma son `Unknown`.
- Los pases se aplican solo al compilar (`-o`); `emit ir` no comprueba. Los tipos del AST siguen siendo texto.
- Los structs se pasan como puntero a slots `int64_t`: una semántica de valor completa (copias, alias en llamadas) es hueco conocido (`# selfhost-xfail:`).

No describas el selfhost como equivalente al arranque en lo que el subplan lista como pendiente.

## Cortes

Los módulos de `selfhost/` siguen los cortes del arranque: lexer, parser, tipos, ownership, efectos, lugares, IR, fusión, emisión. `1 - 2 - 3` tiene el mismo árbol que en `src/parser.rs`. `selfhost/drops.sal` es el espejo de `src/drops.rs` (`free`, `list_free`, `dict_free`, `enum_free`, `bytes_free` de locales; `drops_body(f, variants)` recibe el mapa de variantes que arma `fuse.sal`); `lower_fn` de `fuse.sal` lo aplica al cuerpo antes de bajar, y cualquier cambio en un lado exige el mismo en el otro (la IR de `PARITY_FILES` lo comprueba). En **`selfhost/fuse.sal`**, `FO_COLUMN` y `collect_fused_binary` reflejan `FusedOp::ColumnBin` del arranque. El parser escrito en sal debe reconocer `frame` cuando el arranque lo haga.

Módulos de `selfhost/` hoy: `lexer`, `parser`, `ast`, `attrs`, `checks`, `effects`, `device`, `infer`, `tshape`, `ownership`, `qualify`, `modules`, `ir`, `fuse`, `tensorops`, `emit`, `main` (más `cache`, `handles`, `models`, `result`). Un pase nuevo del arranque en `src/<x>.rs` debería tener su `selfhost/<x>.sal`; atributos (`src/attrs.rs` ↔ `selfhost/attrs.sal`), calificadores (`src/qualify.rs` ↔ `selfhost/qualify.sal`) y dispositivos (`src/device.rs` ↔ `selfhost/device.sal`) ya son pares.

En el parser, `parse_if` coincide con `src/parser.rs`: acumula `elsif` con `parse_elsif_arms` mientras `p_peek` sea `TK_ELSIF()`, y `else` solo si después es `TK_ELSE()`; si no, `ex_if(..., else_b = 0, arms)` y el resto del pipeline ya trata el bloque `0` como vacío. La bajada anida cada `elsif` como un `if` en la rama else.

Las primitivas de tensor no se reimplementan en sal. Se llaman y el runtime del arranque las resuelve.

`std/prelude.sal` define `Option`, `Result` y las firmas de `List`, `map`, `reduce`, `matmul`, `softmax`, `reshape`, `transpose`, `relu`, `load` y `print`. `load` se escribe `load[F32, 4, 4](path)`. La E/S del compilador vive en `std/io/`. En un proyecto externo esa copia está en `sal_modules/io/` y el import es `import "io/io.sal"`.

**Imports y paquetes (`selfhost/modules.sal`)**: debe coincidir con `src/modules.rs` y `src/packages.rs`. Escanea `sal_modules/` del proyecto, `Sal.toml` (`path` y nombre de dependencia) y `Sal.lock` (`name` + `version` → `$SAL_PACKAGES` o `~/.sal/packages`). No invoca git ni red; `sal fetch` materializa la caché antes de compilar con stage1/stage2. Tras cambiar la resolución, `cargo test --test selfhost_ir selfhost_resolves_git` y los tests `fetch_*` de `toolchain`.

## Corpus

`corpus/` cubre, como mínimo: un movimiento legal, efectos declarados, un `to` explícito, un `forward` del que se mira `peak_bytes`, un `load` de `.salt`, **`corpus/column_ops.sal`** (operadores columnares en `on`, región `ColumnBin`) y un programa pensado para `--instrument`.

## Tres cadenas

```text
bootstrap Rust  → compila selfhost/  → stage1
stage1          → compila selfhost/  → stage2
bootstrap, stage1 y stage2 compilan cada fichero de corpus/
```

La IR normalizada de cada programa del corpus es idéntica en las tres. Normalizar quita nombres frescos que no sean estructurales y deja operaciones, lugares, `fused` y `peak_bytes`.

Stage1 sale de compilar `selfhost/` con el arranque. Stage2 sale de compilar `selfhost/` con stage1. Ninguno reimprime la IR del otro.

`selfhost/` compilado con `--instrument` termina sin `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN` ni `INF`. Compilar no exige GPU ni TPU.

## Pruebas que miden la paridad

En `tests/selfhost_ir.rs`:

| Test | Qué fija |
|------|----------|
| `selfhost_ir_matches_bootstrap_on_parity_files` | La IR del selfhost coincide con la del arranque en todo `.sal` del repo que el arranque baja (incluido `selfhost/`) |
| `selfhost_binaries_behave_like_bootstrap_binaries` | Mismo código de salida y stdout en ejemplos y corpus (salvo `examples/infer/tiny_*`, que imprimen tiempos) |
| `selfhost_meets_language_conformance` | Cumple `tests/lang/*.sal`; salta los que llevan `# xfail:` o `# selfhost-xfail:` |
| `selfhost_matches_bootstrap_on_every_repo_program` | Compara ambos compiladores en `examples/`, `corpus/`, `tests/`, `std/`, `benches/` |
| `selfhost_resolves_git_package_import`, `selfhost_import_*`, `selfhost_instrumented_clean_on_hello`, `selfhost_model_package_tests_exit_zero` | Módulos, paquetes, instrumentación, modelos |

Un hueco que solo es del selfhost se marca en el `.sal` de `tests/lang/` con `# selfhost-xfail: motivo`; ver [sal-conformidad](../sal-conformidad/SKILL.md). Quitar el hueco al arreglarlo.

## Cierre

`tests/selfhost_ir.rs` compara las tres IR y falla si una difiere. No marcar el criterio como cumplido con un stub.

Cada ampliación del lenguaje que este compilador implementa lleva tests de ese funcionamiento en el mismo cambio. El corpus o un test del pase tiene que ejercitar la construcción nueva; coincidir la IR de programas que no la usan no cuenta como prueba.

`selfhost/` también pasa por `standard check` / `fix`. Esa limpieza no sustituye el test de IR. Si una práctica del compilador escrito en sal se repite y `standard` no la cubre, se amplía el linter en el mismo cambio; ver [sal-standard](../sal-standard/SKILL.md).

Los enums del selfhost salen por el camino AST→C de `emit.sal` (`emit_c_match`, `emit_c_variant_make`), no por el IR de `fuse.sal`, que todavía no baja `match` ni constructores de variante; usan la misma caja `[tag, campos…]` del runtime.
