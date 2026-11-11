---
name: sal-memoria-asan
description: >-
  Comprueba la gestión de memoria de sal con clang ASan, LeakSanitizer y UBSan:
  sal --sanitize, tests/memory_asan.rs, tests/memory/, las marcas
  `# asan-xfail:` y los controles negativos tests/support/asan_neg_*.c. Usar al
  añadir o cambiar un test de memoria, al sospechar una fuga, un doble free o un
  uso tras liberar, al triar un informe de LeakSanitizer, o al cerrar un hueco
  de liberación y quitar su `asan-xfail`.
---

# Memoria bajo sanitizers

Oráculo **independiente** de `--instrument`: ese cuenta lo que pasa por `sal_xmalloc`; ASan ve cualquier `malloc`/`free`, el uso tras liberar y los desbordes, aunque el contabilizador propio se equivoque. Criterio de qué libera cada pase: [sal-memoria](../sal-memoria/SKILL.md).

## Cómo se ejecuta

```bash
cargo run --release -- --sanitize build f.sal -o f   # también SAL_SANITIZE=1
ASAN_OPTIONS=detect_leaks=1 ./f                       # informe en stderr; el test fija exitcode=99
cargo test --test memory_asan -- --test-threads=1     # el barrido completo (~25 s)
scripts/test-area.sh memory                           # lo mismo; es un área del CI
```

- `--sanitize` (global) compila el IR y el runtime C y enlaza con `-fsanitize=address,undefined`, en `-O0` y con `-g`. Los objetos van en caché aparte (`module.asan.o`, `obj_cache_path` en `src/incremental.rs`) y los del runtime incluyen los flags en su clave. Con `--target soso` es `E_TARGET`.
- Código: `sanitize_enabled` y `sanitize_flags` en `src/compile.rs`. Es una variable de entorno y no un campo de `CompileOptions` para no tocar las decenas de sitios que lo construyen.
- Límites: las páginas de `@arena` salen de `mmap` y ASan no las ve (sí las `malloc`); gpu y tpu son emulación en host.

## Qué cubre `tests/memory_asan.rs`

| Test | Qué hace |
|------|----------|
| `sanitized_programs_manage_memory` | Compila con `--sanitize` cada programa con `# exit:` de `tests/lang/{drops,ownership,collections,structs,enums,strings,methods,generics}` y de `tests/memory/`. Exige el exit y el stdout de la cabecera y **ningún** `Sanitizer` ni `runtime error:` en stderr |
| `sanitizer_catches_deliberate_memory_bugs` | Controles negativos `tests/support/asan_neg_{leak,double_free,use_after_free}.c`, enlazados con `runtime/sal_runtime.c`: el sanitizer tiene que detectarlos (exit 99). Si dejan de saltar, el barrido pasaría en falso |

- Se saltan los ficheros sin `# exit:` (diagnósticos) y los que llevan `# xfail:` (hueco del frontend: no corren).
- Si clang no puede ejecutar ASan (p. ej. ptrace restringido), el test se salta con aviso; con `SAL_REQUIRE_ASAN=1` falla. El CI lo exige.
- Salida en `target/test-asan/`; cada binario se borra tras ejecutarse (cuota de disco).

## `tests/memory/`: casos dedicados

Programas con la misma cabecera que `tests/lang` (`# exit:`, `# stdout:`), escritos a mano para estresar lo que los tests de IR no ejecutan: bucles de 10 000 iteraciones de alloc/free, move en una sola rama, reasignación, structs con campos con dueño. Hoy: `loop_string_churn`, `loop_list_of_strings`, `loop_struct_with_owned_fields`, `conditional_move`. Añade uno por cada patrón de dueño nuevo. Pasan `standard check` como cualquier `.sal`.

## Huecos conocidos: `# asan-xfail: motivo`

Misma convención que `xfail` y `selfhost-xfail`: el programa **debe seguir fallando** bajo el sanitizer; si queda limpio, el test falla hasta que se quite la marca (`: asan-xfail (…) but it is clean now`). El motivo es la causa concreta, no «pendiente». Inventario (`grep -rn '^# asan-xfail' tests/lang tests/memory`):

| Causa | Programas |
|-------|-----------|
| struct/enum devuelto por una función de usuario y consumido en `main` no se libera | `drop_enum_deep`, `drop_struct_containers`, `drop_struct_nested_strings`, `enum_owns`, `generic_struct` |
| `Dict.get` de un valor struct: el `Option` libera su caja, pero la copia (`desc_copy`) no la libera nadie (`sal_enum_free` solo conoce `String` y enum) | `dict_owned_values` |
| `n = p.name` mueve un campo `String` a un local y ese local no se libera | `partial_move_ok` |
| `String` por valor a una función genérica (tipo borrado): ni el llamador ni la callee lo liberan | `generic_two_params` |
| `int_to_str` como clave de `Dict.remove`/`put` en bucle | `dict_remove_many` |
| el `String` de `f64_bits_to_str` | `method_float_math` |

## Triar un informe

1. Reproduce con `--sanitize build` y mira `Direct leak … #3 <función del runtime> #4 … main`: la función de alocación (`sal_strdup`, `sal_enum_new`, `sal_dict_get`, `sal_struct_new`, `sal_u_<fn>` = función de usuario) dice qué valor se escapó.
2. Mira la IR con `sal emit ir f.sal`: busca el `sal_strdup`/`sal_dict_get` sin su `sal_free`/`sal_enum_free` y qué local lo ligó (`__tmpN` = temporal que levanta el `Hoist`).
3. Arréglalo en el pase dueño (`src/drops.rs`, `Hoist` o `fresh_kind`) y **espeja en `selfhost/ir/drops.sal`** en el mismo cambio.
4. Quita el `# asan-xfail:` del programa y pasa `memory_asan`, `drops`, `lang` e `instrument_sal`.

## Trampas

- Un control negativo que fuga con un puntero vivo en una variable de `main` no se detecta: LSan lo ve «alcanzable». Anula el único puntero (`volatile`) o fuga en un frame ya muerto. Por eso `asan_neg_leak.c` usa `malloc` directo.
- Un doble `sal_list_free` lo reporta ASan como `heap-use-after-free` (la segunda llamada lee la cabecera antes de liberar), no como `double-free`.
- `cargo test --test memory_asan` en paralelo con otros tests que enlazan comparte `rt-cache/`: córrelo con `-- --test-threads=1`.
- ASan en `-O0` multiplica el tiempo: no amplíes las áreas barridas sin necesidad.
