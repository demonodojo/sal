---
name: sal-standard
description: >-
  Linter de estilo standard (standard/, escrito en sal): check y fix in situ.
  Usar al editar .sal, al aplicar buenas prácticas, al ampliar una regla que
  todavía no existe, al editar cops o fixtures, o tras cambios en
  str_concat/str_append, strdup, else+if anidado.
---

# Standard (linter de estilo)

Herramienta aparte de `sal fmt` y de `selfhost/`. **No** cambia la gramática ni los códigos `E_*` del compilador. Reescribe texto fuente con parches por bytes sobre tokens propios (`standard/lexer.sal`).

Las buenas prácticas de los `.sal` se aplican con `standard`, no solo a ojo. `check` señala lo que no cumple; `fix` reescribe el fichero cuando el aviso es la práctica acordada, y el diff se revisa.

Si una práctica se repite y ninguna regla la cubre, se le da esa capacidad a `standard` en el mismo cambio: cop, entrada en `run_all_cops`, fixtures y test. No dejarla como nota ni corregirla solo a mano. El linter puede crecer.

## Binario

Tras `cargo build --bins`:

- `target/debug/standard` (wrapper Rust en `src/bin/standard.rs` que compila y ejecuta `standard/main.sal`).

```bash
cargo run --bin standard -- check path/to/file.sal
cargo run --bin standard -- fix path/to/file.sal   # reescribe el fichero in situ
```

Salida de `check`: avisos en stderr (código tipo `Style/DupString`). Exit distinto de 0 si hay infracciones. Con `--error-format json`, una línea JSON por aviso, con `code`, `message`, `span` (`start`/`end` en bytes) y `fix` (texto de reemplazo, o `null` si no hay arreglo). La extensión usa ese `fix` como quick fix.

## Reglas actuales

| Cop | Qué hace |
|-----|----------|
| `Style/DupString` | `strdup("…")` → `d"…"` |
| `Style/Elsif` | `else` + bloque con un solo `if` → `elsif` (casos ambiguos solo avisan) |
| `Style/StringPlus` | `str_concat` / `str_append` → `+`; cadenas de `var = str_append(var, …)` en varias líneas → una asignación con `+` |
| `Style/PlusLiteral` | `d"…"` operando de `+` → `"…"` (el `+` ya copia; no hace falta dup en constantes) |

`sal fmt` **no** aplica estas sustituciones; para estilo de API de runtime usar `standard fix` o corregir a mano y dejar `standard check` limpio. La extensión de VS Code muestra los avisos de `standard check` junto a los `E_*`. Si el aviso trae reemplazo, la bombilla lo aplica; `SAL: Standard Fix` aplica `fix` al buffer entero.

## Layout

| Ruta | Rol |
|------|-----|
| `standard/main.sal` | CLI `check` / `fix`, orquesta cops |
| `standard/lexer.sal` | Tokens con spans; comentarios fuera del flujo |
| `standard/patch.sal` | Avisos, parches sin solape, punto fijo |
| `standard/cop_*.sal` | Una regla por fichero |
| `standard/fixtures/` | Entrada/salida esperada |
| `tests/standard.rs` | Integración (compila `main.sal` una vez; `--test-threads=1`) |

Al añadir o cambiar una regla: cop nuevo, enchufar en `run_all_cops` de `main.sal`, fixtures `_in.sal` / `_out.sal`, test en `tests/standard.rs`.

## Verificar

```bash
cargo test --test standard -- --test-threads=1
```

Tras editar cops, comprobar idempotencia: `fix` sobre `_out.sal` no debe cambiar nada.

Ver también [sal-verificar](../sal-verificar/SKILL.md) y [sal-fuente](../sal-fuente/SKILL.md).
