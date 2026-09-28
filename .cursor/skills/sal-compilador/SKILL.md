---
name: sal-compilador
description: >-
  Guía los cambios del compilador bootstrap de sal en Rust y del runtime C:
  orden de pases, archivo dueño y test que cierra el cambio. Usar al editar
  src/, runtime/ o tests/, al binario standard, o al implementar lexer, parser,
  inferencia, ownership, efectos, lugares, IR, fusión, LLVM, frame, operadores
  columnares sobre tensores, instrumentación, caché o la CLI.
---

# Compilador bootstrap

El arranque está en Rust (`src/`, binario `sal`, lib `sal_compiler`). Clang compila el LLVM IR textual. El compilador escrito en sal no sustituye estos pases: los valida. Ver [sal-autohospedaje](../sal-autohospedaje/SKILL.md).

## Orden de pases

`parse` → `infer_program` → `check_effects` → `check_ownership` → `check_devices` → `TypedProgram` → `lower_program` → `fuse_module` → LLVM → Clang.

No saltar un pase ni comprobar en un pase posterior lo que ya tiene dueño.

## Quién toca qué

| Cambio | Archivos | Test |
|--------|----------|------|
| Tokens, layout, sangría (`frame`, …) | `src/lexer.rs` | `tests/frontend.rs` |
| Árbol de la gramática (`Item::Frame`, …) | `src/parser.rs`, `src/ast.rs`, `src/span.rs` | `tests/frontend.rs`, `tests/parse_and_fmt.rs` |
| Texto canónico, JSON del AST | `src/fmt.rs`, `src/ast.rs` | `tests/parse_and_fmt.rs` |
| Tipos, formas, lugares, `borrow`/`take`; tensores en binops; `frame`→struct; `where` | `src/infer.rs`, `src/typed.rs`, `src/modules.rs` | `tests/semantics.rs` |
| Moves, efectos, dispositivos; frame con `String` y `to` | `src/ownership.rs`, `src/effects.rs`, `src/device.rs`, `src/diag.rs` | `tests/semantics.rs` |
| SSA, `TensorBin`/`TensorUnary`, `ColumnBin`, fusión, `peak_bytes` | `src/ir.rs`, `src/fuse.rs` | `tests/forward_ir.rs` |
| Heaps, kernels, `load`, `.salt`; `sal_tensor_bin_f32`, `sal_tensor_cmp_f32`, … | `runtime/sal_runtime.c`, `runtime/sal_runtime.h`, `runtime/kernels.c`, `src/llvm.rs` | `tests/kernels.rs`, `tests/exec_matmul.rs`, `tests/exec_column.rs`, `tests/exec_salt.rs` |
| Bajada y enlace | `src/llvm.rs`, `src/compile.rs` | `tests/run_hello.rs` |
| Sondas, solo con `--instrument` | `runtime/instrument.c`, `src/llvm.rs` | `tests/instrument_events.rs`, `tests/instrument_leak.rs`, `tests/instrument_sal.rs` |
| CLI, módulos, caché | `src/main.rs`, `src/compile.rs`, `src/incremental.rs` | `tests/toolchain.rs`, `tests/run_hello.rs` |
| Linter `standard` (wrapper + tests) | `src/bin/standard.rs`, `standard/*.sal` | `tests/standard.rs` (`--test-threads=1`) |

La extensión de VS Code llama a `sal check --error-format json`, a `sal fmt` y a `standard check` / `standard fix`. No reimplementa pases. El linter de estilo vive en `standard/` (sal). Las buenas prácticas de fuente se aplican con él, y si falta una regla que va a repetirse se le añade ahí; detalle en [sal-standard](../sal-standard/SKILL.md).

Añadir algo al lenguaje exige tests de ese comportamiento en el mismo cambio, en el fichero de la tabla. El test ejecuta la construcción nueva y comprueba el resultado (árbol, diagnóstico con el `E_*` exacto, tipo, IR o ejecución). Un test que solo sigue pasando con el código viejo no cuenta.

## Parser

Cada producción recursiva por la izquierda es un bucle que crece el hijo izquierdo. El operando derecho es el no terminal de la capa siguiente, nunca el mismo. `parse_unary` y `try` sí se llaman a sí mismos.

`sal fmt` imprime el mismo árbol que el compilador acepta y es idempotente: formatear dos veces da el mismo texto. El esquema JSON del AST es el de ida y vuelta (`schema_version` vigente); un campo nuevo entra en el mismo cambio que el nodo del AST. `token_eq` en el parser debe incluir cada keyword unitario nuevo (p. ej. `Frame`).

`frame`: mismas reglas de sangría que `struct` (`skip_newlines`, `Dedent` opcional). En inferencia, `frame_as_struct` registra el struct equivalente y `TypeEnv.frames` para `where` y reglas de lugar.

Operadores columnares: la bajada emite `IrInst::TensorBin` / `TensorUnary` (no `Binary` escalar). Dentro de `on`, la cola del bloque se baja entera; `collect_fused_ops` acumula `FusedOp::ColumnBin`. LLVM reserva buffer y llama a `sal_tensor_*` en `emit_main` (F32 en el corte actual).

`if`: la SPEC permite `elsif` y omitir `else`. En el AST, `Expr::If.elsifs` es la lista de ramas y `else_block` es `Option<Block>` (`None` sin `else`). `parse_if` acumula `elsif` en un bucle y solo consume `else` si el siguiente token lo es; ambos se asocian al `if` interno que acaba de cerrar su suite. Sin `else`, inferencia da `Unit`; la bajada a IR anida cada `elsif` como un `if` en la rama else, y la rama vacía sigue siendo el valor `0`. El espejo en sal está en `selfhost/parser.sal` (`parse_if`, `parse_elsif_arms`, `else_b = 0`).

## Semántica que no se relaja

- El compilador no inserta `to` ni copias host↔dispositivo. Mezclar lugares es `E_PLACE`.
- `on tpu kernel` es `E_DEVICE`. La TPU solo ejecuta la vía masiva.
- Aritmética con wrap. Límites de `List` sí se comprueban.
- Lambda de una sentencia: no es un valor que se guarde en `let` ni se devuelva (`E_TYPE` en el arranque).
- Sin la flag `--instrument` el binario no lleva sondas, tampoco en `--release`.

## Caché

Clave: hash del AST tipado, versión del compilador y flags que cambian el binario (`--release`, `--instrument`, `--device`). Un acierto reutiliza el `.o` y marca `cache_hit`. No regenerar el objeto para «asegurar» el resultado.
