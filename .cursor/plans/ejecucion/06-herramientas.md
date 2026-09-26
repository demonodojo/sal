# Plan: Herramientas (06)

**Alcance fijo:** `src/main.rs`, `src/compile.rs`, `src/incremental.rs`, `src/lib.rs`, `tests/run_hello.rs`, `tests/selfhost_ir.rs`, `Sal.toml`.

**No tocar:** lexer/parser/infer/ownership/device/effects/ir/fuse/llvm/runtime (solo invocar APIs públicas).

**Firmas públicas a preservar:** `cache_key`, `cache_path`, `is_cache_hit`, `store_cached_meta` (+ `compile_*` ya exportadas en `lib.rs`).

**Depende de:** pipeline semántica+IR lo bastante estable para `hello` (ya) y luego forward.  
**Desbloquea:** B11, orquestación B13–B14.

---

## T01 — CLI completa y flags

- **Archivo:** `src/main.rs`
- **Hacer:**
  - Subcomandos: `check`, `build`, `run`, `fmt`, `emit ast|typed|ir|llvm` (ya).
  - Flags: `--release`, `--device cpu|gpu|tpu`, `--instrument`, `--instrument-out PATH`.
  - `--error-format json` en check/build/run: imprimir `Diagnostic` serializado.
  - `E_DEVICE_MISSING` si `--device gpu|tpu` y el toolchain/driver no está: **build/run** fallan; **check** sigue OK (solo semántica).
- **Hecho:** `sal check examples/hello.sal` exit 0; `sal run` propaga código del binario.

## T02 — Cablear `--instrument-out`

- **Archivo:** `src/main.rs`, `src/compile.rs`
- **Hacer:** al lanzar el binario, `SAL_INSTRUMENT_OUT`; asegurar link con `-DSAL_INSTRUMENT=1` (ya parcial). Documentar en ayuda clap.
- **Hecho:** test de instrumentación (otro agente) puede depender de este env.

## T03 — Pipeline de compilación por módulo

- **Archivo:** `src/compile.rs`
- **Hacer:**
  - Orden fijo: parse → infer → effects → ownership → devices → typed → cache lookup → lower → fuse → emit_llvm → clang → link runtime.
  - Respetar `skip_link`.
  - Si cache hit y existe objeto: **no** re-emitir clang `-c` (hoy siempre linkea de cero salvo skip_link).
- **Hecho:** `tests/run_hello.rs` `incremental_cache_second_build` verifica `cache_hit` **y** mtime/object reuse (p.ej. comparar hash de `.o` o flag interno).

## T04 — Caché incremental real

- **Archivo:** `src/incremental.rs`
- **Hacer:**
  - Clave = hash(AST tipado + versión compilador + flags release/instrument/device) — ya casi.
  - Guardar meta **y** path del `.o` / IR fusionada en `target/incremental/`.
  - Invalidar módulo + importadores cuando cambie un dependiente (grafo).
- **Hecho:** segunda compilación sin tocar fuente → `is_cache_hit == true` y compile más corta / sin reescribir `.o`.

## T05 — `Sal.toml` y resolución de `import`

- **Archivos:** `Sal.toml`, `src/compile.rs`, `src/main.rs`
- **Hacer:** leer `[package]` y `[dependencies]` path locales; resolver `import std/prelude` o paths del paquete a ficheros `.sal`; compilar en orden topológico.
- **Hecho:** proyecto mínimo con dos módulos y `import`; `sal check` OK. Actualizar `Sal.toml` del repo raíz al paquete `sal` / examples sin romper hello.

## T06 — `lib.rs` exports estables

- **Archivo:** `src/lib.rs`
- **Hacer:** reexportar lo que tests y selfhost necesiten sin cambiar nombres públicos listados en el encargo.
- **Hecho:** `cargo test` sigue compilando contra `sal_compiler::*`.

## T07 — Tests de ejecución y caché

- **Archivo:** `tests/run_hello.rs`
- **Hacer:**
  - Mantener `hello_runs_42` (exit 42).
  - Endurecer `incremental_cache_second_build`.
  - Añadir `forward_ir_via_compile` opcional (solo check/skip_link) si no choca con `forward_ir.rs`.
- **Hecho:** `cargo test --test run_hello` verde.

## T08 — Andamiaje triple IR (sin mentir)

- **Archivo:** `tests/selfhost_ir.rs`
- **Hacer:** dejar de comparar archivo consigo mismo. Estructura:
  1. `ir_bootstrap(path)` = lower+fuse vía crate Rust.
  2. `ir_stage1(path)` = invocar binario selfhost si existe; si no, `#[ignore]` o skip documentado hasta B14.
  3. Assert igualdad cuando los tres existan.
  - Leer lista de `corpus/*.sal` cuando exista (plan 07).
- **Hecho:** test falla si bootstrap≠bootstrap (trivial) y **está listo** para comparar tres cadenas; no dar falso verde de autohospedaje.

## Fuera de este plan

- Escribir `selfhost/*.sal` y `corpus/` → plan 07 / superficie.
- Implementar sondas → plan 05.
- Extensión de VS Code → plan 08. Esta CLI ya es el contrato (`check --error-format json`, `fmt` a stdout).
