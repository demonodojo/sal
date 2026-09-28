# Subplan: `String` con cabecera

## Entrada

Runtime y backends actuales: `String` es `char*` con longitud en tabla hash global ([04-ir-runtime.md](04-ir-runtime.md)).

## Salida

Literales y heap con bloque `[cap: i64][len: i64][bytes…][0]` alineado a 16; puntero al valor = `bytes`. Sin tabla global. `String` solo en `cpu`; puente `str_bytes` → `Tensor[I8, ?] on cpu` para gpu.

## Inventario (fase 0)

### Productores `char*` (runtime)

| Función | Notas fase 2 |
|---------|----------------|
| `sal_argv`, `sal_getenv`, `sal_realpath`, `sal_read_file`, `sal_exec_capture`, `sal_tmp_path` | `sal_str_from_cstr` (libc) |
| `sal_strdup`, `sal_str_concat`, `sal_str_append`, `sal_str_slice`, `sal_int_to_str`, `sal_char_to_str`, `sal_select_str` | cabecera heap |
| `sal_lex_src` (tokens) | `sal_str_alloc` |
| `sal_ir_text` | buffer IR; cabecera al finalizar |

### Consumidores que necesitan `len`

Todas las `sal_str_*`, `sal_map_*`/`sal_dict_*` (strcmp hoy), `sal_write_file`, `sal_print_str`.

### `sal_map_put` / `sal_dict_put`

Guardan el puntero de la clave **sin copiar**; el dueño del `String` debe vivir mientras el mapa exista.

### `free` en `selfhost/`

Todos los `free(...)` activos son sobre `String` (tokens, piezas temporales, paths). `vec_free`/`sal_vec_free` son aparte.

### `check_devices` (`src/device.rs`)

`type_place` solo devuelve lugar para `Tensor`; `String` en `on gpu` o `to gpu` **no** se rechaza hoy → fase 4a.

El autohospedaje no ejecuta `check_devices`; la guardia vive en el bootstrap Rust.

## Baseline (referencia)

Medición local orientativa con caché caliente: `sal build selfhost/main.sal` ~0,01s user (caché incremental). Repetir tras fase 2/3 sin regresión perceptible en compilación de `selfhost/`.

Contador `sal_str_find`: eliminado con la tabla (fase 2).

## Contrato

Ver [SPEC.md](../../SPEC.md) sección Memoria (`String`).

## Parallel (fase 3)

- `cap == 0`: literal rodata, lectura concurrente sin lock.
- `cap > 0`: un dueño; mover entre tareas, no compartir mutable.
- `g_arena` (modo sin instrumentación): `_Thread_local`.
- `--instrument` + `parallel` futuro: lock en instrumentación (fuera de este subplan).

## Archivos

| Fase | Archivos |
|------|----------|
| 0 | `SPEC.md`, este documento |
| 1 | `src/llvm.rs`, `selfhost/emit.sal` |
| 2 | `runtime/sal_runtime.c`, `runtime/sal_runtime.h` |
| 3 | `runtime/sal_runtime.c`, `tests/runtime_str_threads.rs`, `tests/support/` |
| 4a | `src/device.rs`, `tests/semantics.rs` |
| 4b | `SPEC.md`, `std/prelude.sal`, `src/infer.rs`, `src/ir.rs`, `src/llvm.rs`, `runtime/`, tests |

Tests: `tests/selfhost_ir.rs`, `tests/run_hello.rs`, `tests/semantics.rs`, `tests/gpu_kernel.rs` (4a).

## Hecho cuando

- Literales con cabecera en LLVM y C del selfhost; IR del corpus idéntica en tres cadenas.
- Runtime sin tabla hash; `len` por cabecera.
- TSan limpio en arnés de hilos + literal compartido.
- `to gpu` sobre `String` → `E_PLACE`; `str_bytes` + `to gpu` + `reduce` coherente con la SPEC.
