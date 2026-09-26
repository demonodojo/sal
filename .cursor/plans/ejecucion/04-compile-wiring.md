# Cableado compile.rs para matmul / .salt (agente de herramientas)

Este documento deja los pasos exactos que **no** se aplicaron en `src/compile.rs` / `src/main.rs` (propiedad de otro agente). La IR, fusión, LLVM textual, `kernels.c` y `sal_load_f32` ya están listos.

## Estado actual

- `lower_program` + `fuse_module` producen una `FusedRegion` por bloque `on`, con `Matmul` + `MapEpilogue` (relu/map) y `peak_bytes` / `peak_symbolic` desde formas.
- `emit_llvm` declara `@sal_matmul_f32` y emite `call void @sal_matmul_f32(...)` cuando la IR tiene la primitiva (función host stub o instrucción `Call`).
- El camino `hello` sigue siendo `ConstInt` + `Return` → `ret i64 42`.
- Runtime: `sal_matmul_f32` (mosaico), `sal_softmax_f32` (orden de índice), `sal_load_f32` (magic `SALT`).

## Qué falta en compile / link

Para que un `.sal` con `matmul` en CPU **ejecute** de verdad (no solo IR/LLVM textual):

1. **En `link_binary` (o equivalente en `compile.rs`)**  
   Enlazar siempre, además del `.ll` generado:
   - `runtime/kernels.c` (o su `.o`)
   - `runtime/sal_runtime.c` (o su `.o`)
   - `-lm` (softmax / math)

   Ejemplo de línea Clang (conceptual):

   ```bash
   clang -O0 generated.ll runtime/kernels.c runtime/sal_runtime.c -lm -o bin
   ```

   Con `--release`: `-O3` en lugar de `-O0`. Con `--instrument`: añadir `runtime/instrument.c` y `-g` fuera de release (ya previsto en el plan).

2. **Bajar dims reales a la llamada LLVM**  
   Hoy `emit_llvm` usa placeholders `i64 0` para `m,k,n` cuando no hay formas en la instrucción. El agente de compile debe:
   - Monomorfizar formas estáticas en la IR/`Call`, **o**
   - Pasar longitudes en tiempo de ejecución para ejes `?`,
   - y emitir `call void @sal_matmul_f32(ptr %a, ptr %b, ptr %out, i64 %m, i64 %k, i64 %n)` con esos valores.

3. **Reservar buffers de tensores en el host**  
   Antes del `call`, el codegen debe emitir `malloc`/`sal_*` de heap cpu para activaciones y salida (los pesos prestados no se re-reservan). La región fusionada no reserva el intermedio del epílogo relu.

4. **`load` → `sal_load_f32`**  
   Cuando la IR tenga `Call` a `sal_load` / `load`, bajar a:

   ```llvm
   declare ptr @sal_load_f32(ptr, ptr)
   ; path string global + alloca i64 elems
   %buf = call ptr @sal_load_f32(ptr @path, ptr %out_elems)
   ```

   Enlazar `sal_runtime.c` (ya implementa el contenedor little-endian).

5. **Regiones `on gpu` / `on tpu`**  
   No expandir a `@sal_matmul_f32` del host: enrutar al backend de dispositivo / heap por actor. Solo CPU usa el kernel de `kernels.c`.

6. **Smoke de integración (agente herramientas)**  
   - `examples/hello.sal` → exit 42 (no regresión).
   - Programa mínimo `matmul` 2×2 en cpu con asserts numéricos (mismo resultado que `tests/support/matmul_test.c`).
   - `load` de un `.salt` escrito como en `tests/exec_salt.rs` + forward fusionado.

## Archivos tocados por el subplan IR (referencia)

| Archivo | Rol |
|---------|-----|
| `src/ir.rs` | SSA + `FusedRegion` + peak por formas |
| `src/fuse.rs` | Conserva `MapEpilogue` fusionado |
| `src/llvm.rs` | `declare`/`call` `@sal_matmul_f32` |
| `runtime/kernels.c` | matmul mosaico + softmax ordenado |
| `runtime/sal_runtime.c` | `sal_load_f32` SALT |
| `tests/forward_ir.rs` | región única + epílogo + peak simbólico |
| `tests/kernels.rs` | numérico matmul |
| `tests/exec_salt.rs` | lectura contenedor `.salt` |

**No editar** desde este cableado: `runtime/sal_runtime.h` (firmas congeladas).
