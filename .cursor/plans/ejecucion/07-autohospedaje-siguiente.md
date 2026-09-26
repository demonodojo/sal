# Plan: Autohospedaje — siguiente (07)

**Hecho en superficie (parsea hoy):** `std/prelude.sal`, `examples/{hello,forward,gpu_roundtrip,forward_salt}.sal`, `selfhost/main.sal`, `corpus/{moves,effects,places,forward_fused,load_salt,instrument_oob,instrument_leak}.sal`.

**No tocar desde este plan de superficie:** `src/`, `runtime/`, `tests/` (salvo ampliar corpus en tests cuando exista).

---

## Brechas de sintaxis (bloquean el criterio del plan maestro)

1. **Lambdas** — `map(m, x => x * 2.0)` no parsea (`=>` solo en `match`). `gpu_roundtrip.sal` usa `map(m, double)` con `fn double`.
2. **`load[F32, 4, 4](path)`** — los enteros de forma no son tipos; forma aceptada: `load[Tensor[F32, 4, 4] on cpu](path)`. Hace falta type-args de `load` con elem+dims (comentario en `parser::parse_call_args`).
3. **Enums / structs** — no hay items `enum`/`struct`; `Option`/`Result`/`List` solo como `Named` en firmas del preludio.
4. **Indexación** — `xs[i]` se interpreta como type-args; OOB del corpus usa `index(xs, 99)`.
5. **E/S real** — no hay `read_file`/`write_file` en el runtime de arranque; `selfhost/main.sal` es esqueleto (`read_source` devuelve el path, `emit_ir_text` una cadena fija).

## Tres cadenas (bootstrap → stage1 → stage2)

| Cadena | Qué falta |
|--------|-----------|
| **Bootstrap** | Compilar `selfhost/` a binario nativo con IR estable; tests `tests/selfhost.rs` sobre corpus. |
| **Stage1** | Ese binario recompila `selfhost/`; misma IR normalizada en cada `corpus/*.sal`. |
| **Stage2** | Stage1 recompila `selfhost/`; IR de corpus idéntica a bootstrap y stage1. |

Bloqueadores: parser (1–4), runtime IO + `Result`/`try` real, IR/`fuse` con `peak_bytes` real, CLI `emit ir` normalizado, y `--instrument` limpio sobre `selfhost/`.

## Criterio de cierre

- Corpus parsea y las tres cadenas emiten IR byte-igual (tras normalizar).
- `examples/forward_salt.sal` carga `.salt` y el producto coincide numéricamente.
- `selfhost/` bajo `--instrument` sin LEAK/OOB/…; programas corpus de fuga/OOB fallan con el código esperado.
