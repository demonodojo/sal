# AGENTS.md — desarrollo de sal

Compilador del lenguaje **sal** (ownership inferido, CPU/GPU/TPU en el tipo). Dos compiladores conviven:

- **Arranque (bootstrap)** en Rust: `src/`, binario `sal`, lib `sal_compiler`, runtime C en `runtime/`. Clang compila el LLVM IR.
- **Autohospedaje** en sal: `selfhost/`. Valida al arranque; no lo sustituye.

Las guías detalladas por área están en `.claude/skills/` (enlace a `.cursor/skills/`). Léelas antes de tocar el área correspondiente.

## Autoridad

1. [SPEC.md](SPEC.md) es la gramática y manda sobre todo.
2. [docs/subplans/](docs/subplans/) fija archivos, algoritmo y tests de cada pase (el estado del selfhost está en `07-selfhost.md`).
3. Si SPEC y código divergen, no arregles uno a ojo: avisa al usuario y cierra la divergencia en el mismo cambio.
4. No ampliar la superficie del lenguaje porque un diseño la mencione: lo reservado es `E_PARSE` hasta su fase.
5. Códigos de error cerrados: `E_PARSE`, `E_TYPE`, `E_MOVED`, `E_BORROW`, `E_EFFECT`, `E_PLACE`, `E_SHAPE`, `E_TENSOR_ELEM`, `E_DEVICE`, `E_DEVICE_MISSING`, `E_OOM`, `E_TARGET` (solo destino), `E_INTERNAL` (solo fallos del compilador). `E_BORROW` es un préstamo que se solapa, una referencia que sobrevive a su referente, o un lifetime elidido sin origen. Avisos, que no son errores ni paran la compilación: `W_DEPRECATED` (uso de un item `@deprecated`). No inventes códigos ni eventos de instrumentación.
6. La gramática es recursiva por la izquierda (`1 - 2 - 3` = `(1 - 2) - 3`); prefijos y lambda asocian a la derecha.
7. **Modelo de memoria: el de Rust, más `@arena`.** Un valor con dueño se mueve por defecto; `&T` y `&mut T` prestan (con lifetimes y elisión de Rust); la copia profunda es `x.clone()` explícito; y cada valor se libera al salir de su ámbito con la glue de su tipo (`drop.<Tipo>`). No hay modos `borrow`/`take` inferidos, ni copias implícitas, ni heurísticas de valor «fresco». `@arena` es la única extensión. Si el código o un test dice otra cosa, es una divergencia: se avisa y se cierra (ver [SPEC.md](SPEC.md) «Memoria» y [docs/subplans/13-ownership-rust.md](docs/subplans/13-ownership-rust.md)).

## Estructura

| Ruta | Contenido |
|------|-----------|
| `src/` | Compilador bootstrap (lexer, parser, infer, ownership, effects, device, drops, ir, fuse, llvm, compile, packages…) |
| `runtime/` | Runtime C (`sal_runtime.c`, `kernels.c`, `instrument.c`, `soso/`) |
| `selfhost/` | Compilador escrito en sal (espejo de los pases de `src/`) |
| `std/` | Biblioteca estándar y preludio en sal (`std/prelude.sal`, `std/io`, `http`, `json`, `text`) |
| `standard/` | Linter de estilo `standard`, escrito en sal |
| `corpus/`, `examples/` | Programas de paridad y ejemplos |
| `tests/` | Tests de Rust; `tests/lang/<área>/*.sal` (conformidad, un subdirectorio por área), `tests/sal/` (tests de `std`) |

Orden de pases (objetivo, subplan 13): `parse` → `expand_interpolation` → `infer_program` → `attrs` → `check_effects` → `check_borrows` → `check_devices` → `TypedProgram` → `elaborate_drops` → `lower_program` → `fuse_module` → LLVM → Clang. Hoy el arranque aún ejecuta `check_ownership` e `insert_drops` en esos puestos mientras se cierra el subplan 13; no añadas lógica de memoria nueva a esos pases heredados. No saltes un pase ni compruebes en uno posterior lo que ya tiene dueño.

## Reglas de cambio

- **Todo cambio de memoria se contrasta con Rust**: antes de implementarlo, la regla correspondiente tiene que existir en la SPEC igual que en Rust (mover, préstamo, lifetime, `clone`, glue). Un test de memoria que describa el modelo viejo se migra en el mismo cambio, con el IR o la salida esperados fijados a mano según la SPEC.
- **Todo cambio de lenguaje toca, en el mismo cambio**: la SPEC, el subplan del pase, el pase dueño y los tests que ejercitan lo nuevo. Un test que sigue pasando con el código viejo no cuenta.
- Un diagnóstico nuevo se afirma con su código `E_*` exacto; un programa válido, con el árbol, tipo, IR o ejecución exactos.
- Si la construcción vive en los dos compiladores, el test del arranque no sustituye al del selfhost. Un pase nuevo de `src/<x>.rs` debería tener su `selfhost/<x>.sal`.
- Hueco conocido de conformidad: se marca en la cabecera de `tests/lang/*/*.sal` (`# xfail: motivo` para el arranque, `# selfhost-xfail: motivo` para el selfhost) con motivo concreto. Al arreglarlo, **quita la marca** en el mismo cambio. Antes de afirmar que algo está soportado, mira si su test la lleva (`grep -rl xfail tests/lang`).
- Los resultados esperados de `tests/lang` se escriben a mano según la SPEC; no se copian de la salida de un compilador.
- No describas el selfhost como equivalente al arranque en lo que el subplan lista como pendiente.
- Los métodos (`method`/`static`/`init`, los de tipos básicos como `s.len()`, `xs.push(v)`, y `List.new[T]()`) están en los dos compiladores: los cubren `tests/lang/methods` y `selfhost_meets_language_conformance`.

## Estilo de fuente `.sal`

- Sangría de espacios (un tabulador es `E_PARSE`), comentarios con `#`, última expresión del bloque es el valor.
- Al programar en sal, prefiere `method` y `static` a las funciones, tanto al definirlos como al llamarlos. Definición: `method T.f()` mejor que `fn f(t: T)` cuando hay receptor, y `static T.f()` mejor que `fn f(...)` cuando no lo hay y `T` es un tipo propio del módulo. Llamada: `s.len()` mejor que `str_len(s)`, `xs.push(v)` mejor que `list_push(xs, v)`, `List.new[Int]()` mejor que `list_new()`. Vale también para `selfhost/`.
- Un struct por fichero, y el nombre del fichero es el del struct en snake_case (`TypeEnv` → `type_env.sal`). En el fichero, el struct con sus `method` y `static` va al principio; las funciones, al final. Agrupa los ficheros en carpetas y subcarpetas por función, con un nombre que diga de qué van.
- Todo `.sal` editado pasa por `standard check`; usa `standard fix` y revisa el diff. Si una práctica se repite y ninguna regla la cubre, amplía `standard` en el mismo cambio (cop + `run_all_cops` + fixtures `_in/_out` + test en `tests/standard.rs`).
- `sal fmt` es idempotente y no aplica las reglas de `standard`.

## Compilar y ejecutar

```bash
cargo build --release          # sal y standard en target/release/
cargo run -- check|fmt|run|build <fichero.sal>
cargo run -- emit ast|ir <fichero.sal>
cargo run -- test              # tests/sal/test_*.sal
cargo run -- selfhost          # stage1 (arranque) y stage2 (stage1 compila selfhost)
cargo run --bin standard -- check|fix <fichero.sal>
```

Flags: `--release`, `--device cpu|gpu|tpu`, `--instrument`, `--target linux|soso`, `--error-format json`.

## Verificar (el test del pase tocado, no la suite entera por costumbre)

| Se tocó | Comando |
|---------|---------|
| Lexer / parser | `cargo test --test frontend --test parse_and_fmt` |
| Tipos, ownership, efectos, lugares | `cargo test --test semantics` |
| Drops | `cargo test --test drops` |
| IR / fusión | `cargo test --test forward_ir` |
| Runtime / kernels | `cargo test --test kernels --test exec_matmul --test exec_column --test exec_salt` |
| LLVM / enlace | `cargo test --test run_hello` |
| Instrumentación | `cargo test --test instrument_events --test instrument_leak --test instrument_sal` |
| CLI / módulos / caché / paquetes | `cargo test --test toolchain --test run_hello` |
| Comportamiento del lenguaje | `cargo test --test lang` (+ `selfhost_meets_language_conformance` si el selfhost lo implementa) |
| Builtins mal usados | `cargo test --test builtin_errors` |
| Destino linux/soso | `cargo test --test soso_target --test cli_target --test std_io_linux` |
| `std/` | `cargo run -- test` |
| `standard/` | `cargo test --test standard -- --test-threads=1` |
| `selfhost/`, preludio, `corpus/` | `cargo test --test selfhost_ir -- --test-threads=1` |

Una tarea no está hecha hasta que pasa el test del pase y cubre lo añadido. `cargo test --locked` completo es la puerta de un release.

## Trampas de entorno

- `cargo test` en paralelo hace fallar ~11 tests de `tests/selfhost_ir.rs` por interferencia: córrelos con `-- --test-threads=1` (igual `tests/bins.rs` y `tests/standard.rs`).
- `target/` pesa varios GB y los tests de selfhost dejan temporales; la cuota de disco se llena. Antes de borrar nada en `target/` o `/tmp`, avisa al usuario.
- Sin `--instrument` el binario no lleva sondas, tampoco con `--release`.
- Huecos de liberación cerrados y pendientes: `.cursor/skills/sal-memoria/SKILL.md`.

## Release

Solo cuando el usuario lo pida: versión en `Cargo.toml` = tag `vX.Y.Z`, `cargo test --locked`, commit `Release sal X.Y.Z`, tag anotado y `gh release create`. No hay workflow ni `cargo publish`. Detalle en `.claude/skills/sal-release/SKILL.md`. No toques `std/io/Sal.toml` ni el repo `opencode-sal`.

## Git

Commits y pushes solo cuando el usuario los pida. No incluyas cambios ajenos a la tarea (hay trabajo en curso en `selfhost/`).
