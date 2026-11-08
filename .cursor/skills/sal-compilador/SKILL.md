---
name: sal-compilador
description: >-
  Guía los cambios del compilador bootstrap de sal en Rust y del runtime C:
  orden de pases, archivo dueño y test que cierra el cambio. Usar al editar
  src/, runtime/ o tests/, al binario standard, o al implementar lexer, parser,
  inferencia, ownership, efectos, lugares, IR, fusión, LLVM, frame, operadores
  columnares sobre tensores, atributos, módulos y paquetes, destinos
  (linux/soso), instrumentación, caché o la CLI.
---

# Compilador bootstrap

El arranque está en Rust (`src/`, binario `sal`, lib `sal_compiler`). Clang compila el LLVM IR textual. El compilador escrito en sal no sustituye estos pases: los valida. Ver [sal-autohospedaje](../sal-autohospedaje/SKILL.md).

## Orden de pases

`parse` → `expand_interpolation` (`src/interp.rs`) → `infer_program` → `attrs` (`check_layout_structs`, `check_stack_lets`, `check_no_heap_fns`) → `check_effects` → `check_borrows` (objetivo; hoy `check_ownership`) → `check_devices` → `TypedProgram` → `elaborate_drops` (objetivo; hoy `insert_drops`) → `lower_program` → `fuse_module` (fusión de regiones +, si aplica, `finish_module_box_shell_drops`) → LLVM → Clang.

Con módulos, `src/compile.rs` resuelve el grafo (`resolve_module_graph`), pasa `check_module_semantics` / `infer_module` por módulo y enlaza un objeto por módulo con el runtime. El destino (`--target linux|soso`, `src/target.rs`) entra en la inferencia: `syscall` fuera de soso/linux es `E_TARGET`.

No saltar un pase ni comprobar en un pase posterior lo que ya tiene dueño.

## Quién toca qué

| Cambio | Archivos | Test |
|--------|----------|------|
| Tokens, layout, sangría (`frame`, …) | `src/lexer.rs` | `tests/frontend.rs` |
| Árbol de la gramática (`Item::Frame`, …) | `src/parser.rs`, `src/ast.rs`, `src/span.rs` | `tests/frontend.rs`, `tests/parse_and_fmt.rs` |
| Texto canónico, JSON del AST | `src/fmt.rs`, `src/ast.rs` | `tests/parse_and_fmt.rs` |
| Tipos, formas, lugares, `borrow`/`take`; tensores en binops; `frame`→struct; `where` | `src/infer.rs`, `src/typed.rs`, `src/modules.rs` | `tests/semantics.rs` |
| Moves, efectos, dispositivos; frame con `String` y `to` | `src/ownership.rs`, `src/effects.rs`, `src/device.rs`, `src/diag.rs` | `tests/semantics.rs` |
| Atributos `@frozen`/`@stack`/`@no_heap`/`@layout(c)` | `src/attrs.rs`, `src/layout.rs`, `src/lexer.rs`, `src/parser.rs` | `tests/lang/attributes/attr_*.sal` vía `tests/lang.rs` |
| Interpolación `f"a#{x}"` | `src/interp.rs`, `src/string_expr.rs` | `tests/lang/<área>/*.sal`, `tests/run_hello.rs` |
| Calificadores, alias, símbolos `modid__nombre` | `src/qualify.rs`, `src/modules.rs` | `tests/lang/modules/mods/`, `tests/toolchain.rs` |
| `match` escalar (`"…"`, `r"…"`, brazo bloque), `RSTRING` en lexer | `src/lexer.rs`, `src/parser.rs`, `src/ast.rs`, `src/infer.rs` (`check_match_patterns`), `src/ir.rs` (`lower_match_chain`, `match_should_use_switch`), `src/fmt.rs`, `runtime/sal_runtime.c` (`sal_regex_match`) | `tests/lang/control_flow/match_*.sal`, `tests/lang/control_flow/err_match_*.sal`, `tests/lang/syntax/err_regex_outside_match.sal`, `tests/forward_ir.rs` (`match_*`), `tests/parse_and_fmt.rs` (`fmt_match_*`), `tests/soso_target.rs` (`soso_rejects_regex_match_pattern`) |
| Métodos de tipos básicos (`x.sqrt()`, `s.len()`), mate de `Float` (`float_*`, libm, `E_TARGET` en soso) | `src/methods.rs` (tabla), `src/infer.rs`, `src/llvm.rs`, `runtime/kernels.c` | `tests/lang/methods/method_*.sal`, `tests/lang/numeric/err_float_math_*.sal`, `tests/semantics.rs` |
| Builtins mal usados (aridad, tipo del argumento) | `src/infer.rs` | `tests/builtin_errors.rs` (siempre `E_TYPE`) |
| Destino linux/soso, `syscall` | `src/target.rs`, `runtime/soso/linux_host_syscall.c`, `src/llvm.rs` | `tests/soso_target.rs`, `tests/cli_target.rs`, `tests/std_io_linux.rs` |
| Comportamiento del lenguaje de extremo a extremo | cualquier pase + `runtime/` | `tests/lang/<área>/*.sal` (ver [sal-conformidad](../sal-conformidad/SKILL.md)) |
| Liberación de locales y cola de struct (`insert_drops`; `struct_free_box`; cola `List[struct]` en `field_drop`; `sal_struct_free_box` en runtime) | `src/drops.rs`, `src/ir.rs`, `src/fuse.rs`, `runtime/sal_runtime.c` | `tests/drops.rs`, `tests/lang/drops/`, `tests/instrument_sal.rs`; espejo `selfhost/ir/drops.sal`, `selfhost/ir/fuse.sal`, `selfhost/ir/frc.sal` — [sal-memoria](../sal-memoria/SKILL.md) |
| SSA, `TensorBin`/`TensorUnary`, `ColumnBin`, fusión, `peak_bytes`, caja de `list_get` tras fusión | `src/ir.rs`, `src/fuse.rs` (`fuse_module`, `fuse_module_opts`) | `tests/forward_ir.rs`, `tests/drops.rs` |
| Heaps, kernels, `load`, `.salt`; `sal_tensor_bin_f32`, `sal_tensor_cmp_f32`, … | `runtime/sal_runtime.c`, `runtime/sal_runtime.h`, `runtime/kernels.c`, `src/llvm.rs` | `tests/kernels.rs`, `tests/exec_matmul.rs`, `tests/exec_column.rs`, `tests/exec_salt.rs` |
| Bajada y enlace | `src/llvm.rs`, `src/compile.rs` | `tests/run_hello.rs` |
| Sondas, solo con `--instrument` | `runtime/instrument.c`, `src/llvm.rs` | `tests/instrument_events.rs`, `tests/instrument_leak.rs`, `tests/instrument_sal.rs` |
| CLI, módulos, caché | `src/main.rs`, `src/compile.rs`, `src/incremental.rs`, `src/modules.rs` | `tests/toolchain.rs`, `tests/run_hello.rs` |
| Paquetes git, lock, `sal_modules/` | `src/packages.rs`, `src/modules.rs` | `tests/toolchain.rs` (`fetch_*`, `install_*`, `sal_modules_import`, `import_not_*`), `tests/selfhost_ir.rs` (`selfhost_resolves_git_package_import`) |
| Linter `standard` (wrapper + tests) | `src/bin/standard.rs`, `standard/*.sal` | `tests/standard.rs`, `tests/bins.rs` (`--test-threads=1`) |
| `som2salt` (export de modelos `.som` → `.salt`) | `src/bin/som2salt.rs`, `src/som_export.rs` | `tests/som_export_errors.rs`, `tests/bins.rs` |

La extensión de VS Code llama a `sal check --error-format json`, a `sal fmt` y a `standard check` / `standard fix`. No reimplementa pases. El linter de estilo vive en `standard/` (sal). Las buenas prácticas de fuente se aplican con él, y si falta una regla que va a repetirse se le añade ahí; detalle en [sal-standard](../sal-standard/SKILL.md).

Añadir algo al lenguaje exige tests de ese comportamiento en el mismo cambio, en el fichero de la tabla. El test ejecuta la construcción nueva y comprueba el resultado (árbol, diagnóstico con el `E_*` exacto, tipo, IR o ejecución). Un test que solo sigue pasando con el código viejo no cuenta.

## Parser

Cada producción recursiva por la izquierda es un bucle que crece el hijo izquierdo. El operando derecho es el no terminal de la capa siguiente, nunca el mismo. `parse_unary` y `try` sí se llaman a sí mismos.

`sal fmt` imprime el mismo árbol que el compilador acepta y es idempotente: formatear dos veces da el mismo texto. El esquema JSON del AST es el de ida y vuelta (`schema_version` vigente); un campo nuevo entra en el mismo cambio que el nodo del AST. `token_eq` en el parser debe incluir cada keyword unitario nuevo (p. ej. `Frame`).

`frame`: mismas reglas de sangría que `struct` (`skip_newlines`, `Dedent` opcional). En inferencia, `frame_as_struct` registra el struct equivalente y `TypeEnv.frames` para `where` y reglas de lugar.

Operadores columnares: la bajada emite `IrInst::TensorBin` / `TensorUnary` (no `Binary` escalar). Dentro de `on`, la cola del bloque se baja entera; `collect_fused_ops` acumula `FusedOp::ColumnBin`. LLVM reserva buffer y llama a `sal_tensor_*` en `emit_main` (F32 en el corte actual).

`if`: la SPEC permite `elsif` y omitir `else`. En el AST, `Expr::If.elsifs` es la lista de ramas y `else_block` es `Option<Block>` (`None` sin `else`). `parse_if` acumula `elsif` en un bucle y solo consume `else` si el siguiente token lo es; ambos se asocian al `if` interno que acaba de cerrar su suite. Sin `else`, inferencia da `Unit`; la bajada a IR anida cada `elsif` como un `if` en la rama else, y la rama vacía sigue siendo el valor `0`. El espejo en sal está en `selfhost/front/parser.sal` (`parse_if`, `parse_elsif_arms`, `else_b = 0`).

## Semántica que no se relaja

- El compilador no inserta `to` ni copias host↔dispositivo. Mezclar lugares es `E_PLACE`.
- `on tpu kernel` es `E_DEVICE`. La TPU solo ejecuta la vía masiva.
- Aritmética con wrap. Límites de `List` sí se comprueban.
- Lambda de una sentencia: no es un valor que se guarde en `let` ni se devuelva (`E_TYPE` en el arranque).
- Sin la flag `--instrument` el binario no lleva sondas, tampoco en `--release`.

## Liberación

El criterio (qué se suelta, la cola de los campos de un struct, la invariante y los huecos) está en [sal-memoria](../sal-memoria/SKILL.md). El pase de drops es `elaborate_drops` en el objetivo (subplan 13) y `insert_drops` hoy; va entre `TypedProgram` y `lower_program`. El modelo que manda es el de Rust (SPEC «Memoria»), más `@arena`.

## Caché

Clave: hash del AST tipado, versión del compilador y flags que cambian el binario (`--release`, `--instrument`, `--device`). Un acierto reutiliza el `.o` y marca `cache_hit`. No regenerar el objeto para «asegurar» el resultado.

En el bucle multi-módulo de `compile_file`, cada `.sal` baja y fusiona por separado. Módulos cuyo path contiene el componente `selfhost` fusionan **sin** el pase IR de caja (`fuse_module_opts(..., false)`); el resto usa `fuse_module` completo. `sal emit ir` y `bootstrap_ir_text` siempre fusionan con caja activa.

## Módulos y paquetes

Resolución de `import` (Rust y sal): directorio del fuente → raíz del proyecto → `sal_modules/` del proyecto → `path` en `[dependencies]` de `Sal.toml` → entradas de `Sal.lock` en la caché global (`$SAL_PACKAGES` o `~/.sal/packages/<nombre>-<versión>/`). Un path `clave/resto` enlaza `sal_modules/clave/resto`, o la clave del manifiesto, o el `name` del lock. El texto del import no depende de dónde esté instalado sal.

`sal fetch [--update]` resuelve tags `vX.Y.Z` (semver caret en el manifiesto), clona en la caché y escribe `Sal.lock`. `sal install [--update]` hace `fetch` y enlaza cada dependencia en `sal_modules/<nombre>`. `sal add <nombre> --git URL [--version REQ]` edita el manifiesto y ejecuta `fetch`. `check` y `build` no usan red; paquete ausente en caché → `E_TYPE` (`import not found`). La descarga solo la hace la CLI de Rust; `selfhost/pkg/modules.sal` lee lock y caché ya materializados. Detalle en [sal-paquetes](../sal-paquetes/SKILL.md).

## Otras órdenes de la CLI

`sal test [dir]` compila y ejecuta cada `test_*.sal` (por defecto `tests/sal`); ver [sal-stdlib](../sal-stdlib/SKILL.md). `sal selfhost` compila `selfhost/` con el arranque y luego con ese binario (stage2); no está soportado con `--target soso`. `sal compile --ast` acepta el JSON de `sal emit ast`.

`sal parity <fichero.sal> [--selfhost <bin>]` compara la IR que produce el arranque (la misma que `sal emit ir`) con la de un binario selfhost, por defecto `target/sal-out/selfhost-stage2` (hay que haber ejecutado `sal selfhost` antes). Si coinciden imprime `parity ok` y sale con 0; si no, imprime la primera línea que difiere de cada lado y sale con 1; si falta el binario, sale con 2. Es el primer paso para localizar una divergencia de IR antes de abrir `tests/selfhost_ir.rs`.
