# Plan: IR / runtime (04)

**Alcance fijo:** `src/ir.rs`, `src/fuse.rs`, `src/llvm.rs`, `runtime/kernels.c`, `runtime/sal_runtime.c`, `tests/forward_ir.rs`, `tests/kernels.rs`.

**No tocar:** `runtime/sal_runtime.h` (instrumentación solo añade prototipos al final); frontend; semántica; `instrument.c`; CLI salvo que compile ya enlace (el enlace vive en herramientas — aquí solo producir LLVM/runtime correctos).

**Firmas públicas a preservar:** `lower_program`, `fuse_module`, `ir_to_text`, `emit_llvm`.

**Depende de:** B03–B04 (tipos/lugares). Puede bajar desde AST ya parseado.  
**Desbloquea:** B05–B09 → instrumentación, ejemplos numéricos, selfhost.

---

## T01 — Enriquecer IR SSA

- **Archivo:** `src/ir.rs`
- **Hacer:** ampliar `IrInst` (sin romper serialización útil) con al menos:
  - `Copy` / `Move` / `Borrow` / `Drop`
  - `Alloca { dest, ty, place }`
  - `OnDevice { region_id o place, args, dest }`
  - `TensorOp { op: Matmul|Softmax|Map|Reduce|Reshape|Transpose|Relu, … }`
  - `ConstFloat`, literales string si hace falta para `load`
  - Metadatos de forma/elem en ops fusionables (`rows`, `cols`, `elem_bytes`)
- **Hecho:** `lower_program` de `forward.sal` produce región + instrucciones coherentes; test snapshot parcial en `tests/forward_ir.rs`.

## T02 — Lowering completo de expresiones usadas

- **Archivo:** `src/ir.rs`
- **Hacer:** bajar `Binary`/`Unary`/`Float`/`Bool`/`String`/`Assign`/`Let`/`Call`/`On`/`To`/`TensorLit`; no tragar con `tN` vacío. `On` crea `FusedRegion` **y** un `OnDevice`/lanzamiento único tras fusión.
- **Hecho:** IR de `hello` = const + return; IR de `forward` = región con matmul(+epílogo).

## T03 — Fusión real: una región, sin intermedio

- **Archivo:** `src/fuse.rs` (+ cooperación `ir.rs`)
- **Hacer:**
  - Fusionar `map`/`relu` pegado a `matmul` en epílogo; **eliminar** del stream de instrucciones la materialización del intermedio.
  - Una sola región por bloque `on`.
  - Plan de buffer de trabajo: tensores unique que mueren en la región reutilizan slot; pesos borrowed no se reallocan.
- **Hecho:** `tests/forward_ir.rs`:
  - exactamente 1 `region on`
  - `fused=true`
  - texto IR **no** contiene reserva/op intermedia entre matmul y relu (definir assert: p.ej. no hay `MapEpilogue` separado ni `sal_relu` suelto post-fuse).

## T04 — `peak_bytes` desde formas (sin mágicos)

- **Archivos:** `src/fuse.rs`, `src/ir.rs` (`estimate_peak` eliminar literales 4096/2048/1024)
- **Hacer:** `peak_bytes[place] = max live (pesos + activaciones + scratch)` usando `elem_size * product(dims)`; ejes `?` → usar símbolo o cota documentada en IR (p.ej. omitir o marcar `dyn`).
- **Hecho:** assert en `forward_ir.rs` que el valor de peak para el lugar de la región es `> 0` y **distinto** de la suma fija antigua; idealmente igualdad con fórmula conocida para shapes estáticas de un fixture.

## T05 — Runtime host: heaps y tensores

- **Archivo:** `runtime/sal_runtime.c` (prototipos nuevos → pedir a instrumentación que los añada al final de `sal_runtime.h`, o coordinar en el mismo PR de superficie: **este agente implementa `.c`**; si el header aún no declara, usar decls locales temporales solo hasta que instrumentación sincronice — preferir sync del header vía cola de instrumentación).
- **Hacer:**
  - `sal_alloc(place, nbytes)`, `sal_free(place, ptr)` con heaps lógicos cpu/gpu/tpu (gpu/tpu pueden ser arenas host-emuladas).
  - Representación mínima `SalTensor` (ptr, dims, elem, place).
  - `sal_place_copy(dst_place, src)`.
  - Completar `sal_load_*` según SPEC `.salt` (magic SALT, ver, elem, rank, dims, payload LE).
  - Indexación List con check (error → panic o hook instrumentación).
- **Hecho:** test C o Rust que alloc/free por place y load de fixture `.salt`.

## T06 — Kernels: mosaico + softmax + tipos estrechos

- **Archivo:** `runtime/kernels.c`, `tests/support/matmul_test.c`, `tests/kernels.rs`
- **Hacer:**
  - `sal_matmul_f32` en mosaico (tile fijo documentado) + empaquetado de filas si cabe en el arranque.
  - `sal_softmax_f32` reducción en orden creciente de índice (ya casi).
  - Entradas F16/BF16/I8: ensanchar a F32, llamar núcleo, estrechar si aplica; IR conserva tipo estrecho.
- **Hecho:** `tests/kernels.rs` pasa numérico 2×2; añadir `softmax_order` y un caso F16 o I8 mínimo.

## T07 — LLVM: bajar IR a llamadas de runtime

- **Archivo:** `src/llvm.rs`
- **Hacer:**
  - Emitir **todas** las funciones del módulo, no solo `main`.
  - `ConstInt`/`Return`/`Call` reales; `PlaceCopy` → `sal_place_copy`; región fusionada → una llamada `sal_matmul_f32` (+ epílogo inline o `sal_relu_f32` in-place).
  - Declarar símbolos del runtime.
  - Con `LlvmOptions.instrument`: llamar `sal_instrument_init` al entrar `main`, `shutdown` al salir, y hooks en alloc/free/index/tensor check (los cuerpos los completa instrumentación).
- **Hecho:** `emit_llvm` de `hello` es IR LLVM parseable por clang; de un micro-forward declara `@sal_matmul_f32`.

## T08 — Tests

- **Archivos:** `tests/forward_ir.rs`, `tests/kernels.rs`
- **Casos:**
  1. `forward_single_fused_region` — endurecer asserts (B06).
  2. `forward_peak_bytes_not_magic` — peak ≠ {4096} fijo post-relu-fusion.
  3. `matmul_kernel_numeric` — mantener.
  4. `softmax_kernel_order` — nuevo.
  5. (Opcional) `load_salt_roundtrip` en support C.
- **Hecho:** `cargo test --test forward_ir --test kernels` verde.

## Coordinación con instrumentación

- Cualquier símbolo nuevo (`sal_alloc`, `sal_oob_check`, …) que deba vivir en el header: listarlo; el agente de instrumentación añade **solo prototipos al final** de `sal_runtime.h`.
- No reescribir `instrument.c` aquí.
