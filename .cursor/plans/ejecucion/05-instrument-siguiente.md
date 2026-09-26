# 05-instrument — siguiente (llvm / compile)

El runtime de depuración (`runtime/instrument.c`) ya emite `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN` e `INF` bajo `-DSAL_INSTRUMENT=1`. Falta que el compilador inserte las sondas al bajar IR → LLVM y en el enlace.

## Qué falta para `sal run --instrument`

1. **`src/llvm.rs`**: con `LlvmOptions.instrument`, además de `sal_instrument_init` / `shutdown` en `main`:
   - declarar y llamar `sal_instrument_malloc` / `sal_instrument_alloc` en cada reserva de heap
   - `sal_instrument_free` en cada liberación
   - `sal_instrument_check_index` en indexación de `List` / `Tensor`
   - `sal_instrument_check_ptr` en accesos y al entrar a un heap de actor (`place`)
   - `sal_instrument_check_f32` tras ops de tensor que producen/consumen `F32`
   - (opcional) traza JSON de `alloc` / `free` / `place_copy` / entrada-salida de `on`

2. **`src/compile.rs` / CLI**: `--instrument` ya enlaza `instrument.c` con `-DSAL_INSTRUMENT=1`; asegurar que `--instrument-out` recoja el JSON de stderr (o redirija al fichero).

3. **Tests de extremo a extremo**: un `.sal` con fuga / índice fuera / NaN compilado vía `sal run --instrument`, sin invocar clang a mano sobre `tests/support/*.c`.

## API listo para sondas

| Función | Uso del compilador |
|---------|-------------------|
| `sal_instrument_malloc(size, place, site)` | reserva con zona roja; `place`: 0=cpu, 1=gpu, 2=tpu |
| `sal_instrument_alloc(p, size, place, site)` | registrar puntero ya reservado |
| `sal_instrument_free(p)` | liberar + cuarentena; detecta `DOUBLE_FREE` |
| `sal_instrument_check_index(i, len, site)` | `OOB` si `i ∉ [0,len)` |
| `sal_instrument_check_ptr(p, nbytes, place, site)` | `USE_AFTER_FREE` / `BAD_PLACE` / `OOB` (zona roja) |
| `sal_instrument_check_f32(data, n, site)` | `NAN` / `INF` |
| `sal_instrument_shutdown()` | `LEAK` si quedan bloques vivos |

`site` debe ser el span del fuente (texto corto) para el JSON `{"code":"…","site":"…"}`.
