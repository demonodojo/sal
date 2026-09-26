# Plan: Semántica (03)

**Alcance fijo:** `src/infer.rs`, `src/ownership.rs`, `src/effects.rs`, `src/device.rs`, `src/diag.rs`, `src/typed.rs`, `tests/semantics.rs`.

**No tocar:** lexer/parser (salvo leer AST), IR, LLVM, runtime, CLI.

**Firmas públicas a preservar:** `infer_program`, `check_effects`, `check_ownership`, `check_devices`, `TypedProgram::from_program`.

**Depende de:** B01–B02 (frontend) para fixtures ricos. Puede arrancar con snippets string en tests.  
**Desbloquea:** B03–B04 → IR tipado, preludio.

---

## T01 — Códigos y diagnóstico JSON-ready

- **Archivo:** `src/diag.rs`
- **Hacer:** asegurar `ErrorCode::{EMoved,EEffect,EPlace,EShape,ETensorelem,EDevice,EDeviceMissing,EOom,EParse,EType}` + `as_str` + `Serialize`; método o helper `format_json` si la CLI lo necesita (herramientas lo llamará). Hints en `E_PLACE` / `E_EFFECT` / `E_MOVED`.
- **Hecho:** `tests/semantics.rs` assert `diag.code.as_str() == "E_SHAPE"` etc.

## T02 — `E_TENSOR_ELEM` y elementos de tensor

- **Archivo:** `src/infer.rs` (parser ya rechaza `Float` en `Tensor[…]`; reforzar en infer si el AST llega por JSON)
- **Hacer:** rechazar `Float`/`Int`/`Bool` como elem de tensor; solo F32|F16|BF16|I8.
- **Test:** `tensor_elem_float_is_error` → `E_TENSOR_ELEM`.

## T03 — Formas: `E_SHAPE` en `matmul` / `reshape`

- **Archivo:** `src/infer.rs`
- **Hacer:** mantener/completar `matmul_result_type`; tipar `reshape`/`transpose`/`softmax`/`relu`/`map`/`reduce` con tensores; eje `?` permitido.
- **Test:** `matmul_inner_mismatch` → `E_SHAPE`; `matmul_ok_dynamic_batch` acepta `Tensor[F32, ?, 4]`.

## T04 — `to` reescribe lugar; `E_PLACE`

- **Archivos:** `src/infer.rs`, `src/device.rs`
- **Hacer:**
  - `check_expr` para `Expr::To`: resultado = mismo tensor con `place` destino.
  - `device::expr_place_compatible` / tracking real: dentro de `on p`, args tensor deben estar en `p` (o `Place::Param` unificado).
  - No insertar copias implícitas: solo error.
- **Test:** `mixed_place_without_to` → `E_PLACE`; `to_gpu_then_on_gpu_ok`.

## T05 — `E_DEVICE` (`on tpu kernel`)

- **Archivo:** `src/device.rs`
- **Hacer:** ya hay stub; cubrir con test y rechazar también usos ilegales de kernel en tpu en expresiones anidadas.
- **Test:** `tpu_kernel_is_e_device`.

## T06 — Efectos: `load`, `on gpu`/`tpu`, `tensor[…]`

- **Archivo:** `src/effects.rs`
- **Hacer:** `load` ⇒ `io`+`alloc`; `on gpu`/`to` hacia gpu ⇒ efecto `gpu` (y análogo tpu); `tensor` lit ⇒ `alloc`; `print`/`panic` cuando existan en superficie.
- **Test:** `load_requires_io_alloc` → `E_EFFECT`; `on_gpu_requires_gpu_effect`.

## T07 — Ownership: moves, préstamos de una sentencia, `to` consume

- **Archivo:** `src/ownership.rs`
- **Hacer:**
  - Tipos triviales (`Int`/`Float`/`Bool`/structs triviales) = copy; resto = move.
  - Tras `to place x` o `reshape`/`transpose`, el origen queda moved si era unique.
  - Args de llamada: por defecto take en unique; marcar borrow cuando el cuerpo no consume (inferencia léxica).
  - Eliminar lista mágica `is_move_fn` sustituyendo por tipado/capacidad.
- **Test:** `use_after_to_is_e_moved`; `borrow_param_reusable` (mismo tensor pesos dos veces en cuerpo si borrow).

## T08 — Inferir `ParamMode` y publicarlo en AST tipado

- **Archivos:** `src/infer.rs` / `src/ownership.rs`, `src/typed.rs`
- **Hacer:** rellenar `Param.mode` en la `FnDef` del `TypedFnDef` (clone mutado) como `Borrow` o `Take`.
- **Test:** `forward_w_is_borrow` (pesos no se consumen); parámetro de `reshape` es `Take`.

## T09 — Match / Option / Result / try / List (mínimo para preludio)

- **Archivo:** `src/infer.rs`
- **Hacer:** tipar `match` por unificación de brazos; `try` exige `Result` y propaga; `List[T]` indexación (bounds en runtime, tipo `T`).
- **Test:** `try_on_result_ok`; `match_option_ok`; error de tipo si brazos divergen → `E_TYPE`.

## T10 — Suite `tests/semantics.rs`

- **Archivo:** `tests/semantics.rs` (crear)
- **Casos (todos con strings o `include_str` de examples en RO):**
  1. `E_TENSOR_ELEM`
  2. `E_SHAPE`
  3. `E_PLACE`
  4. `E_DEVICE`
  5. `E_EFFECT`
  6. `E_MOVED`
  7. `E_TYPE` unknown ident
  8. `infer_forward_ok` + `ParamMode`
  9. `check_devices_gpu_roundtrip` (cuando frontend parsea)
- **Hecho:** `cargo test --test semantics` verde; firmas públicas sin cambio de nombre.

## Fuera de este plan

- `peak_bytes`, fusión, LLVM, instrumentación, CLI.
