# Plan: Instrumentación (05)

**Alcance fijo:** `runtime/instrument.c`, `runtime/sal_runtime.h` (**solo** añadir prototipos al final), `tests/instrument_*.rs`, `tests/support/`.

**No tocar:** `kernels.c` / lógica de `sal_runtime.c` (salvo que un test support compile contra ambos); frontend; semántica; `fuse.rs`/`ir.rs` (solo consumir hooks que LLVM ya inserta — si faltan llamadas, listar requisito a IR/LLVM).

**Depende de:** B09 (LLVM inserta llamadas) para tests vía `sal run --instrument`. Hasta entonces, support C puede validar el runtime.  
**Desbloquea:** B10, criterio LEAK/OOB/NAN del plan maestro.

---

## T01 — Prototipos al final de `sal_runtime.h`

- **Archivo:** `runtime/sal_runtime.h`
- **Hacer:** al **final** del header (antes de `#endif`), declarar todo lo que IR/runtime necesite e instrumentación implemente, p.ej.:
  - ya existentes: `sal_instrument_init/shutdown/alloc/free/check_f32`
  - nuevos: `sal_instrument_check_bounds`, `sal_instrument_use`, `sal_instrument_place_check`, `sal_instrument_set_out_path`, y prototipos de `sal_alloc`/`sal_free`/`sal_place_copy` si IR los pide y aún no están.
- **Hecho:** header compila con `kernels.c` + `sal_runtime.c` + `instrument.c`; sin reordenar APIs previas de forma incompatible.

## T02 — Mapa de bloques, cuarentena, zonas rojas

- **Archivo:** `runtime/instrument.c` (`#ifdef SAL_INSTRUMENT`)
- **Hacer:**
  - Cada `sal_instrument_alloc`: registrar ptr, size, place, site (span), stack frames sal si disponible.
  - Padding de zona roja alrededor del bloque; poison en free.
  - Free → cuarentena (no reutilizar inmediato); segunda free del mismo ptr → evento `DOUBLE_FREE` + `exit≠0`.
  - Acceso vía `sal_instrument_use` a ptr liberado → `USE_AFTER_FREE`.
  - Ptr usado con `place` distinto al registrado → `BAD_PLACE`.
- **Hecho:** tests support C (o `.sal` cuando exista) para cada código.

## T03 — Eventos JSON versionados

- **Archivo:** `runtime/instrument.c`
- **Hacer:** emitir a stderr (humano/JSON línea) y, si `SAL_INSTRUMENT_OUT` / API set path, al fichero. Campos mínimos: `schema`, `code` (`LEAK|OOB|USE_AFTER_FREE|DOUBLE_FREE|BAD_PLACE|NAN|INF`), `site`/`span`, `place`, `op` (para NAN/INF).
- **Hecho:** parseable; `LEAK` al shutdown si quedan vivos; programa limpio: shutdown sin evento, exit 0.

## T04 — OOB en List/Tensor e indexación

- **Archivo:** `runtime/instrument.c` (+ prototipo header)
- **Hacer:** `sal_instrument_check_bounds(ptr, index, len, site)` → `OOB` + exit≠0; detectar escritura en zona roja.
- **Hecho:** `tests/instrument_oob.rs` (preferir `sal run --instrument` sobre un `.sal` o support que simule la sonda).

## T05 — NAN / INF en ops de tensor

- **Archivo:** `runtime/instrument.c` (ya parcial `check_f32`)
- **Hacer:** conservar; asegurar que LLVM/kernels llaman el check en modo instrument (coordinar). Evento incluye `op` y `place`.
- **Hecho:** sustituir/complementar `tests/instrument_leak.rs` (hoy C suelto) con:
  - `tests/instrument_nan.rs` vía `sal run --instrument` **o** support C mientras LLVM no enlace; el criterio de aceptación exige vía CLI cuando B09/B11 existan.

## T06 — LEAK con span

- **Archivo:** `runtime/instrument.c`, tests
- **Hacer:** site debe ser el span de reserva (string `file:line:col` o offset); test de fuga alloc sin free.
- **Hecho:** `tests/instrument_leak.rs` assert stderr/out contiene `"code":"LEAK"` y el site; exit≠0. Preferir programa sal `tests/support/leak.sal` + `sal run --instrument`.

## T07 — Traza alloc/free/place_copy/on

- **Archivo:** `runtime/instrument.c`
- **Hacer:** en modo instrument, log opcional de `alloc`, `free`, `place_copy`, enter/leave `on` al fichero de out (mismo esquema).
- **Hecho:** test que un roundtrip gpu genera al menos un `place_copy` en el JSON out.

## T08 — Tests (sin red)

- **Archivos:** `tests/instrument_leak.rs`, `tests/instrument_oob.rs`, `tests/instrument_nan.rs`, `tests/instrument_clean.rs`, `tests/support/*`
- **Casos de aceptación:**
  1. Fuga → `LEAK`, exit≠0
  2. Índice fuera → `OOB`, exit≠0
  3. Tensor con NaN → `NAN`, exit≠0
  4. Programa que libera todo y sin NaN → exit 0, sin eventos fatales
  5. (Extra) `DOUBLE_FREE`, `USE_AFTER_FREE`, `BAD_PLACE` cuando las sondas existan
- **Migración:** dejar de considerar el C suelto como prueba de aceptación; puede quedar como unit del runtime, pero el gate es `sal run --instrument`.
- **Hecho:** `cargo test --test instrument_leak --test instrument_oob --test instrument_nan --test instrument_clean` verde.

## Coordinación

- Pedir a LLVM (plan 04) que emita `call void @sal_instrument_*` en alloc/free/index/tensor y `init`/`shutdown` en `main`.
- Pedir a herramientas (plan 06) que `sal run --instrument` defina `SAL_INSTRUMENT` en el link (ya) y pase `--instrument-out` al env `SAL_INSTRUMENT_OUT`.
