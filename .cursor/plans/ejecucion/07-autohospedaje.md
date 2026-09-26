# Plan: Autohospedaje y superficie sal (07)

**Alcance fijo:** `std/`, `examples/`, `selfhost/`, `corpus/` (crear).

**No tocar:** `src/**`, `runtime/**`, `tests/**` salvo que el agente de herramientas pida fixtures; este plan **define** el contenido sal y el corpus que los tests consumirán. Si hace falta un test nuevo, coordinar con el dueño del test (`selfhost_ir` = herramientas; `forward_ir` = IR; `semantics` = semántica; `frontend` = frontend).

**Invariantes de ejemplos:**
- `examples/hello.sal` debe seguir devolviendo **42**.
- `examples/forward.sal` debe conservar `on` + `relu(matmul(…))` (puede ampliarse con `load` / `main` rico, sin romper esa forma en `forward`).

**Depende de:** B01–B12 (compilador usable).  
**Cierra:** B12–B15.

---

## T01 — Preludio `std/prelude.sal`

- **Archivo:** `std/prelude.sal`
- **Hacer:** definir en sal (no stubs vacíos):
  - `enum Option[T]`, `enum Result[T, E]`
  - operaciones/`fn` de `List` usadas por el arranque
  - tensores: `map`, `reduce`, `matmul`, `softmax`, `reshape`, `transpose`, `relu` (pueden ser builtins reconocidos por el compilador **o** wrappers que llamen al runtime; documentar cuál)
  - `load`, impresión (`print`)
- **Hecho:** `sal check std/prelude.sal` (o módulo que lo importe) OK; examples pueden `import` prelude.

## T02 — `examples/hello.sal`

- **Archivo:** `examples/hello.sal`
- **Hacer:** mantener `fn main() -> Int` / `42`. No añadir efectos innecesarios.
- **Hecho:** `sal run examples/hello.sal` → código salida 42 (`tests/run_hello.rs`).

## T03 — `examples/gpu_roundtrip.sal`

- **Archivo:** `examples/gpu_roundtrip.sal`
- **Hacer:** conservar `on` + `to gpu` / `to cpu` + `scale` con `map`; asegurar que parsea tras B01. `main` puede devolver 0.
- **Hecho:** `sal check` OK; bajo runtime emulado, `sal run` exit 0 (sin exigir GPU física).

## T04 — `examples/forward.sal` + pesos `.salt`

- **Archivos:** `examples/forward.sal`, `examples/w.salt` y/o `examples/x.salt` (o bajo `corpus/`)
- **Hacer:**
  - Función `forward` con `on p` / `relu(matmul(x, w))` (obligatorio).
  - `main` que haga `load` de pesos/entrada, `to gpu` opcional, calcule y termine (exit code estable o print).
- **Hecho:** `sal emit ir` → 1 región fusionada + `peak_bytes`; producto numérico coincide con oracle (test kernels o run).

## T05 — Corpus de IR triple

- **Directorio:** `corpus/` (crear)
- **Programas mínimos:**
  1. `moves.sal` — `E_MOVED` no; uso legal de move
  2. `effects.sal` — `load` con efectos
  3. `places.sal` — `to` / `on`
  4. `forward_fuse.sal` — forward + peak (puede symlink/copy de example)
  5. `load_salt.sal` + `.salt`
  6. `instrument_clean.sal` — libera todo
  7. (opcional) `instrument_leak.sal` — solo para tests de instrumentación, no para igualdad IR de stage si diverge por flags
- **Hecho:** cada fichero parsea; `tests/selfhost_ir.rs` itera el corpus.

## T06 — Compilador `selfhost/` en sal

- **Archivos:** `selfhost/main.sal` y módulos (`lexer`, `parser`, `ast`, `infer`, `ir`, `fuse`, `emit`, `cache`, …) según se pueda con la superficie disponible.
- **Hacer:**
  - Leer fuente, emitir IR textual **compatible** con `ir_to_text` del bootstrap (o formato acordado y normalizado en el test).
  - Efectos `io`/`alloc`; errores vía `Result` + `match`/`try`.
  - Caché incremental en sal.
  - Tests de modelo (`load`, `matmul`, `softmax`, fuse, peak, places) ejecutables por el binario selfhost.
- **Hecho:** bootstrap compila `selfhost` → binario stage1; stage1 compila `selfhost` → stage2; ambos + bootstrap emiten la misma IR normalizada para cada programa del corpus.

## T07 — Selfhost bajo `--instrument`

- **Archivos:** `selfhost/`, coordinación con herramientas
- **Hacer:** `sal build --instrument selfhost/main.sal` (o entry) y ejecutar compilación de un corpus file sin `LEAK|OOB|USE_AFTER_FREE|DOUBLE_FREE|BAD_PLACE|NAN|INF`.
- **Hecho:** exit 0; JSON out vacío de fatales.

## T08 — Criterio de cierre (checklist)

Marcar hecho solo cuando **todo** esto pase en CI local (`cargo test` + runs manuales citados):

| # | Criterio | Evidencia |
|---|----------|-----------|
| 1 | hello → 42 | `tests/run_hello.rs` |
| 2 | forward IR 1 región + peak | `tests/forward_ir.rs` |
| 3 | matmul / `.salt` numérico | `tests/kernels.rs` + example |
| 4 | caché 2ª compilación | `tests/run_hello.rs` |
| 5 | LEAK/OOB/NAN vía `--instrument` | `tests/instrument_*.rs` |
| 6 | limpio instrument exit 0 | `tests/instrument_clean.rs` |
| 7 | IR bootstrap = stage1 = stage2 | `tests/selfhost_ir.rs` + `corpus/` |
| 8 | selfhost instrumentado limpio | comando documentado en README (README lo actualiza quien tenga permiso; aquí solo dejar el comando en este plan) |
| 9 | sin red en tests | grep de tests sin sockets |

## Orden de implementación recomendado

1. T01 prelude (tras frontend+semántica)  
2. T02–T04 examples  
3. T05 corpus  
4. T06 selfhost incremental (puede empezar como emisor IR mínimo)  
5. T07–T08 cierre
