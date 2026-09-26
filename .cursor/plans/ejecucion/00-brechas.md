# Brechas contra el criterio de aceptación

Ordenadas por dependencia (arriba primero). Cada fila: qué falta, archivo(s) a tocar, test concreto que la cierra. El reparto de agentes está fijado; no reasignar archivos.

Criterio de referencia: «Fases posteriores» en `.cursor/plans/lenguaje_sal_26b08325.plan.md` + SPEC.md + subplanes `docs/subplans/02`…`08`.

---

## B01 — Lambdas / cierres de una sentencia (bloquea `map` y ejemplos)

- **Estado:** `examples/gpu_roundtrip.sal` falla con `E_PARSE` en `map(m, x => x * 2.0)` (FatArrow solo en `match`).
- **Archivos:** `src/ast.rs` (`Expr::Lambda`), `src/lexer.rs` (si hace falta), `src/parser.rs`, `src/fmt.rs`, `tests/frontend.rs`.
- **Hecho cuando:** `parse(include_str!("../examples/gpu_roundtrip.sal")).is_ok()` y `fmt` idempotente sobre ese AST.

## B02 — Superficie de tipos del preludio (structs, enums, `List[T]`, patrones)

- **Estado:** AST/parser solo tienen `fn` + `import`; sin `struct`/`enum`; patrones de `match` sin constructores; `List` es `Named` sin semántica.
- **Archivos:** frontend (`ast.rs`, `parser.rs`, `fmt.rs`) → semántica (`infer.rs`, `ownership.rs`).
- **Hecho cuando:** `tests/frontend.rs` parsea un snippet con `enum Option[T]`, `enum Result[T,E]`, `struct`, `match Some(x) => …`; `tests/semantics.rs` tipa `List[Int]` y `try`.

## B03 — Inferencia real: `E_TENSOR_ELEM`, formas, `to` cambia lugar, `ParamMode`

- **Estado:** `infer` acepta `Unknown` en llamadas arbitrarias; `Expr::To` no reescribe el `place`; `ParamMode` siempre `Inferred`; no hay test de `E_SHAPE`/`E_TENSOR_ELEM`/`E_PLACE`.
- **Archivos:** `src/infer.rs`, `src/typed.rs`, `src/diag.rs` (solo si falta formato), `tests/semantics.rs`.
- **Hecho cuando:** tests en `tests/semantics.rs` fallan con código exacto:
  - `Tensor[Float, 2, 2]` → `E_TENSOR_ELEM`
  - `matmul` con K estáticos distintos → `E_SHAPE`
  - mezcla de lugares sin `to` → `E_PLACE`
  - y `TypedProgram` publica `borrow`/`take` en params de `forward`.

## B04 — Ownership y dispositivos no stub

- **Estado:** `expr_place_compatible` siempre `true`; `place_matches` siempre `false`; moves solo para unos pocos nombres hardcodeados; `to` no libera origen de forma tipada.
- **Archivos:** `src/ownership.rs`, `src/device.rs`, `src/effects.rs`, `tests/semantics.rs`.
- **Hecho cuando:** doble uso tras `to gpu x` → `E_MOVED`; `on tpu kernel` → `E_DEVICE`; `load` sin `! io, alloc` → `E_EFFECT`; tensor cpu dentro de `on gpu` sin `to` → `E_PLACE`.

## B05 — IR SSA completa (no solo ConstInt + Call)

- **Estado:** `IrInst` carece de `copy`/`move`/`borrow`/`alloca`/`on_device`/ops de tensor tipadas; binarios/floats/strings no bajan; `Assign` se ignora.
- **Archivos:** `src/ir.rs`, `tests/forward_ir.rs`.
- **Hecho cuando:** `lower_program` de `forward.sal` emite región `on` + `place_copy` si hay `to`, y `ir_to_text` muestra ops SSA legibles (no `Debug` crudo obligatorio, pero sí `fused=` y `peak=`).

## B06 — Fusión elimina intermedio en el stream y `peak_bytes` real

- **Estado:** `fuse.rs` quita `MapEpilogue` de la región, pero las instrucciones siguen siendo `sal_matmul` + `sal_relu`; `peak_bytes` usa constantes mágicas 4096/2048/1024.
- **Archivos:** `src/fuse.rs`, `src/ir.rs` (liveness/formas en ops), `tests/forward_ir.rs`.
- **Hecho cuando:** IR de `relu(matmul(x,w))` = **una** región, **sin** op intermedia materializada, `peak_bytes` por lugar calculado desde elem×forma (no literales mágicos); assert en `tests/forward_ir.rs`.

## B07 — Runtime: heaps por actor, `String`/`List`/`Tensor`, `load` completo

- **Estado:** `sal_runtime.c` solo `print_i64`, `panic`, `load_f32` parcial; sin heaps cpu/gpu/tpu; sin indexación con límites.
- **Archivos:** `src/ir`/`llvm` llaman API; implementación en `runtime/sal_runtime.c` (+ prototipos nuevos solo al final de `sal_runtime.h` vía agente de instrumentación).
- **Hecho cuando:** `tests/kernels.rs` o test de ejecución carga un `.salt` y obtiene el buffer; alloc/free por `place` distinguible.

## B08 — Kernels: `matmul` en mosaico + F16/BF16/I8 + softmax ordenado

- **Estado:** `kernels.c` es triple bucle ingenuo solo F32; no hay ensanche F16/BF16/I8 → F32.
- **Archivos:** `runtime/kernels.c`, `tests/kernels.rs`, `tests/support/matmul_test.c` (y siblings).
- **Hecho cuando:** producto 2×2 conocido pasa; test de softmax verifica orden de reducción; stub o ruta F16 documentada y enlazada.

## B09 — LLVM baja IR real (y no solo `main` + constantes)

- **Estado:** `emit_llvm` ignora funciones ≠ `main`, comenta el resto de `IrInst`, no llama kernels ni `place_copy`, no inserta sondas reales.
- **Archivos:** `src/llvm.rs`, enlace en `src/compile.rs` (herramientas), `tests/run_hello.rs` + tests de forward numérico.
- **Hecho cuando:** `sal run examples/hello.sal` → exit 42 (ya); `sal run` de un forward mínimo con `.salt` produce el tensor esperado (o exit code acordado); LLVM declara y llama `sal_matmul_f32` / `sal_place_copy`.

## B10 — Instrumentación completa + vía `sal run --instrument`

- **Estado:** solo `NAN`/`INF`/`LEAK` parciales; sin OOB/UAF/DOUBLE_FREE/BAD_PLACE/zonas rojas/cuarentena; test usa C suelto, no la CLI.
- **Archivos:** `runtime/instrument.c`, prototipos en `runtime/sal_runtime.h`, `src/llvm.rs` (sondas), `tests/instrument_*.rs`, `tests/support/`.
- **Hecho cuando:** programas `.sal` vía `sal run --instrument` emiten `LEAK`/`OOB`/`NAN` con exit ≠ 0 y span; programa limpio exit 0; sin red.

## B11 — CLI / caché / módulos / Sal.toml

- **Estado:** caché solo escribe meta JSON, no reutiliza objeto `.o`; sin grafo de imports; `Sal.toml` vacío; `--error-format json` y `--instrument-out` no cableados al runtime.
- **Archivos:** `src/main.rs`, `src/compile.rs`, `src/incremental.rs`, `src/lib.rs`, `Sal.toml`, `tests/run_hello.rs`, `tests/selfhost_ir.rs`.
- **Hecho cuando:** segunda `compile_file` con `skip_link` marca `cache_hit` **y** no regenera `.o` si la clave coincide; `import` de un módulo del proyecto resuelve path; diagnóstico JSON estable.

## B12 — Preludio y ejemplos ejecutables

- **Estado:** `std/prelude.sal` es un comentario; `forward.sal` no hace `load` ni calcula; `gpu_roundtrip` no parsea.
- **Archivos:** `std/prelude.sal`, `examples/hello.sal` (seguir → 42), `examples/forward.sal` (`on` + `relu(matmul)`), `examples/gpu_roundtrip.sal`, corpus de pesos `.salt` bajo `examples/` o `corpus/`.
- **Hecho cuando:** `sal run examples/hello.sal` → 42; `sal emit ir examples/forward.sal` muestra región fusionada + `peak_bytes`; roundtrip GPU compila/checkea (ejecución en heap gpu del runtime aunque no haya GPU física).

## B13 — Corpus de autohospedaje

- **Estado:** no existe `corpus/`; `tests/selfhost_ir.rs` compara un archivo consigo mismo.
- **Archivos:** `corpus/*.sal` (+ `.salt` si aplica), `tests/selfhost_ir.rs`.
- **Hecho cuando:** corpus incluye moves, efectos, lugares, forward+peak, load `.salt`, programa para `--instrument`; test compara IR normalizada de bootstrap vs stage1 vs stage2 (aunque stage1/2 aún stub, la API del test debe ser la triple).

## B14 — Compilador `selfhost/` en sal

- **Estado:** `selfhost/main.sal` solo `return 0`.
- **Archivos:** `selfhost/**/*.sal` (superficie sal), orquestación en `tests/selfhost_ir.rs` / CLI (herramientas).
- **Hecho cuando:** bootstrap compila `selfhost/` a binario; ese binario emite IR del corpus idéntica a la del bootstrap; segunda etapa (sal←sal) igual; `selfhost` bajo `--instrument` sin eventos fatales.

## B15 — Cierre de aceptación end-to-end

- **Estado:** piezas anteriores sueltas; no hay prueba única del criterio «Fases posteriores».
- **Archivos:** tests ya citados + posiblemente checklist en `tests/selfhost_ir.rs` / `tests/run_hello.rs`.
- **Hecho cuando:** `cargo test` cubre: parse/fmt/JSON, E_SHAPE/E_TENSOR_ELEM/E_PLACE/E_MOVED/E_EFFECT/E_DEVICE, forward IR fusionado + peak, matmul numérico (+ `.salt`), caché hit, instrumentación LEAK/OOB/NAN vía `sal run --instrument`, IR triple del corpus. Sin llamadas de red.

## B16 — Extensión de VS Code

- **Estado:** no existe `editors/vscode/`. El editor no resalta `.sal`, no sangra bloques ni muestra los `E_*` de `sal check`.
- **Archivos:** `editors/vscode/` (manifiesto, gramática TextMate, configuración de lenguaje, cliente de la CLI, formateador, tests del mapeo). No se toca `src/**` ni `runtime/**`: el compilador ya imprime un `Diagnostic` JSON por línea en stderr.
- **Depende de:** B11 (`sal check --error-format json` y `sal fmt` a stdout). No bloquea B12–B15.
- **Hecho cuando:** un `.sal` se asocia al lenguaje; un doble de `sal check` que emite un `E_PARSE` coloca el diagnóstico en el span; Format Document solo sustituye el buffer si `sal fmt` sale 0 y el texto coincide con esa salida. Tests sin red y con el proceso `sal` sustituido.
