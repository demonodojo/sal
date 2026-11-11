# Subplan: `String` inmutable

La autoridad es [SPEC.md](../../SPEC.md): «Expresiones» (bajada de `+`), «Métodos» (sin `append`), «Memoria» → «`String`», «`Bytes`» y «Frontera de asas». Este subplan fija la auditoría, los archivos, el algoritmo, los tests y las medidas. Si diverge de la SPEC, manda la SPEC.

## Contrato

- Un `String` no cambia después de construirse. Reasignar la variable sí se puede: se liga a otro valor y se suelta el viejo.
- Ninguna operación fuera de la «Frontera de asas» escribe en los bytes de un `String` que alguien pueda leer.
- Un `String` se construye entero en un bloque privado y se entrega cuando ya está terminado.
- Layout y ABI no cambian: `[cap][len][bytes][0]`, alineado a 16; `cap == 0` es un literal en rodata.
- No hay conteo de referencias, GC, COW ni ropes.
- La única reutilización de un bloque es la del acumulador `x = x + …`. La asignación mueve `x`, así que no queda otro dueño ni un préstamo vivo (lo comprueba `check_borrows`), y nadie puede ver el valor viejo. No hace falta un análisis de alias nuevo.

## Auditoría (fase 0)

### Inventario

Clases:

- **A**: solo lectura.
- **B**: construye un valor nuevo.
- **C**: mutación interna segura, sobre un bloque privado o movido.
- **D**: mutación observable.
- **E**: alias o escape de puntero.
- **F**: propiedad o liberación.

| Operación | Dónde | Clase antes | Ahora |
|---|---|---|---|
| `str_len`, `str_eq`, `str_contains`, `str_index_of`/`_from`, `str_char`, `str_skip`, `str_hash`, `str_to_f64_bits`, patrón `STRING`/`RSTRING`, `print_str`, `write_file` | `runtime/sal_runtime.c`; `src/infer.rs` (grupo `Int`) | A | A |
| `strdup` / `d"…"` / `.dup()` / `.clone()` | `sal_strdup` | B | B |
| `+` de dos hojas, `str_concat` / `.concat()` | `sal_str_concat`; `src/ir.rs` `lower_string_chain` | B | B (una reserva exacta) |
| `+` de tres o más hojas (también la interpolación `f"…"`) | antes `sal_str_concat` + `sal_str_append` sobre el temporal | B + C | B: una `sal_str_concat_n` |
| `str_slice` / `.slice()` | `sal_str_slice` copia el rango | B | B (sin lifetime, SPEC) |
| `int_to_str`, `char_to_str`, `f64_bits_to_str`, `read_file`, `argv`, `getenv`, `realpath`, `tmp_path`, `exec_capture`, `ir_text` | `sal_str_from_cstr` / `sal_str_alloc_size` | B | B |
| `bytes_to_str` / `b.to_str()` | `sal_bytes_to_str` copia | B, pero el resultado **no se soltaba** y `b` se daba por movido | B: se suelta; `b` solo se presta |
| `f64_bits_to_str` / `x.to_str()` de `Float` | `sal_str_from_cstr` | B, pero el resultado **no se soltaba** | B: se suelta |
| `str_bytes_copy` / `b.write_str(off, s)` | copia `s` al `Bytes` | A (de `s`) | A |
| `str_bytes(s)` | copia a un búfer de tensor | B | B |
| `dict_get`, `dict_keys`, `dict_values`, `sal_enum_copy`, `desc_copy`, `sal_arena_export` | copian los `String` | B | B (`""` desnudo → `sal_strdup(NULL)`) |
| Tokens del lexer nativo (`lex_push_number`, `lex_unescape`) | `sal_runtime.c` | B, con `sal_strdup` sobre búfer de pila **sin cabecera** | B (`sal_str_from_cstr`; NULL → pánico) |
| **`str_append(s, t)` / `s.append(t)`** | `src/methods.rs`, `src/infer.rs`, `src/borrowck.rs` (`&mut` vía `mutates_container`) | **D**: escribe en el bloque de `s` (todo alias lo ve) o lo libera al crecer (UAF/doble free; `str_append(xs.get_mut(0), …)` deja la lista con un puntero liberado) | Fuera del lenguaje: `E_TYPE` |
| Acumulador `x = x + …` | `src/ir.rs` `assign_accumulates_string`, `src/drops.rs` `is_accumulate` | C, pero **D/UAF** si `x` es `&String` (parámetro o `get`) o si `x` vuelve como hoja (`x = x + a + x`) | C: solo si `x` es dueño (no referencia); `sal_str_append_n` comprueba el solape |
| `str_as_int` / `.to_int()`, `str_from_int`, `str_share`, `list_push_share`, `select_str`, `bytes_ptr`, `bytes_view_mem`, `vec_*`, `map_*`, `syscall` | runtime | E | E, documentado en la «Frontera de asas» |
| `sal_dict_put` | guarda el puntero de la clave sin copiarla | F (la clave se mueve al dict) | F |
| `sal_list_select` | lista dueña con los punteros de otra | E (doble free) | Reportado, fuera de alcance |
| `sal_free`, `drop.String`, `rebind`, `Hoist`, temporales de `+` | `sal_runtime.c`, `src/drops.rs`, `src/ir.rs` | F | F; `add_is_string_chain` decide por tipo |

### Comprobaciones con programas (arranque en `HEAD`, ASan)

| Programa | Antes |
|---|---|
| `t = s.append(…)` y después `s.len()` | Basura (sale 150) y fuga |
| `str_append(xs.get_mut(0), …)` | `heap-use-after-free` |
| `x = x + d"a" + x` | `heap-use-after-free` |
| `fn f(s: &String)` con `s = s + d"x"` | Doble free |
| `t = xs.get(0)` y `t = t + d"x"` | Doble free |
| `s = a + b` (variables) en un `while` | Fuga de cada vuelta: `add_is_string_chain` era sintáctico |
| Llamada a función desconocida | `E_TYPE` (`unknown function`) |
| Llamar `sal_*` desde el fuente | `E_TYPE` |

### Divergencias encontradas y cómo se cierran

| Divergencia | Cierre |
|---|---|
| SPEC «Expresiones» (`+`): «un parámetro pasa a `take`» y «temporal fresco» frente a «Memoria» (sin modos inferidos ni heurística de valor fresco) | Texto reescrito en términos de Rust (el acumulador mueve `x`; los temporales se sueltan). `ParamMode` sigue en el pase heredado hasta cerrar el subplan 13 |
| SPEC «`String»: «`append` copia / `append` in situ si cabe» | Reescrita: inmutabilidad, y reutilización solo para el acumulador |
| SPEC «Métodos»: `append` sin semántica | Quitado; `E_TYPE` |
| SPEC «Memoria»: asas incompletas (solo `str_as_int`, `vec_push`, `map_put`) | Tabla «Frontera de asas» |
| `03-semantics.md` y `04-ir-runtime.md`: «el derecho de `+` puede no ser `String`» frente a SPEC:275 (`E_TYPE`) | Texto corregido |
| `src/drops.rs` `add_is_string_chain` decidía por la sintaxis, frente a SPEC:275 (decide el tipo) | Usa el tipo inferido. El selfhost no tiene tipos por expresión en `drops.sal`: divergencia anotada |
| SPEC «Tipos» (`List`): «`list_get` presta el mismo puntero; copia para una segunda dueña» frente a «Memoria» (`get` es `&T`, sin copia implícita) | **No se cierra aquí** (no es de cadenas): pendiente del subplan 13 |

### Baseline (antes de tocar nada)

Árbol en `51c0a80`, con trabajo ajeno sin commitear en `selfhost/emit/{cenv,emit,lit_pool,tcstate}.sal`.

- Pasan: `semantics`, `lang`, `builtin_errors`, `forward_ir`, `run_hello`, `drops`, `memory_asan`, `standard`.
- `selfhost_ir`: 21 pasan, 1 ignorado y 3 fallan antes del cambio:
  - `corpus_ir_matches_bootstrap_stage1_stage2` y `selfhost_ir_matches_bootstrap_on_parity_files`, los dos por `corpus/drop_ro_params.sal`.
  - `selfhost_instrumented_clean_on_hello` (`LEAK enum_new`).

## Decisiones

1. **`append` sale del lenguaje.**
   - `s.append(t)` es `E_TYPE` («no method append»).
   - `str_append` es `E_TYPE` («unknown function»): `lowering_only_runtime_fn` en `src/infer.rs`, y la misma función en `selfhost/sem/infer.sal`.
   - `sal_str_append`, `sal_str_append_n` y `sal_str_concat_n` siguen en el runtime, solo para la bajada.
   - Migración: `s = s + t`. `Style/StringPlus` la hace sola.
2. **Bajada de `+` en una sola llamada.**
   - Las hojas de la espina izquierda (`string_chain_leaves`) se evalúan de izquierda a derecha, con los literales adyacentes plegados.
   - Dos hojas: `sal_str_concat`. Tres o más: `sal_str_concat_n(n, parts)`, con una reserva exacta y la suma comprobada (pánico al desbordar).
   - Acumulador: `sal_str_append(x, b)` o `sal_str_append_n(x, n, parts)`, que consumen `x`. Reutilizan el bloque si `cap > 0`, cabe y ninguna parte cae dentro. Si no, reservan una vez (×2, mínimo 16), copian y después liberan `x`.
   - Los temporales con dueño se sueltan después de la llamada, con los mismos predicados de antes.
3. **Guarda del acumulador.** `string_expr::accumulates_into` exige que la hoja izquierda no tenga tipo referencia. La usan `ir.rs` y `drops.rs::is_accumulate`. Con `&String`, `s = s + …` liga `s` a un `String` nuevo (SPEC). No hay regla nueva de `E_TYPE`.
4. **LLVM.** El arreglo de partes es un `alloca [k x ptr]` marcado `;;hoist` (`hoist_allocas`): va al bloque de entrada, así que un bucle no hace crecer la pila.
5. **C del selfhost.**
   - Camino AST→C (`emit.sal`): cada hoja va a un temporal antes de la llamada, porque C no ordena los argumentos, y la llamada usa un literal compuesto `(const char*[]){…}`.
   - Camino IR→C (`cir.sal`, `cir_rt`): el mismo empaquetado, con la cuenta en `LL` porque `R_*` no tiene prototipo.
6. **Runtime.**
   - `sal_str_alloc_size` comprueba longitud y capacidad y hace pánico (`"oom"`) si no hay memoria, en lugar de devolver NULL.
   - Una concatenación vacía devuelve el `g_sal_empty` compartido.
   - Arreglos de la auditoría:
     - `lex_push_number` usaba `sal_strdup` sobre un búfer de pila sin cabecera.
     - `""` desnudo en `sal_bytes_to_str`, `sal_str_bytes_copy`, `sal_enum_copy` y `desc_copy`.
     - Faltaba comprobar NULL en `sal_bytes_to_str` y `lex_unescape`.
7. **`drops.rs`: solo se reclasifica lo que ya existía en el pase heredado; no hay reglas nuevas.**
   - `add_is_string_chain` usa `EXPR_TYPES`, así que `s = a + b` con variables se suelta, también en un bucle.
   - `f64_bits_to_str` (`Float.to_str`) y `bytes_to_str` (`Bytes.to_str`) construyen un `String` nuevo: entran en `returns_fresh` y `call_allocates_fresh_string` (y en `dr_returns_fresh` / `call_allocates_fresh_string` del selfhost).
   - `bytes_to_str` solo lee su `Bytes`: entra en `borrows_args` (`dr_borrows`).
   - Antes, el resultado de los dos no se soltaba nunca: `tests/lang/methods/method_float_math.sal` llevaba `asan-xfail`. Con el cambio LSan lo veía solo a veces, según lo que quedaba en la pila. La marca se quita en este cambio.

## Archivos

| Área | Archivos |
|---|---|
| SPEC y subplanes | `SPEC.md`; `docs/subplans/{03-semantics,04-ir-runtime,09-string,13-ownership-rust,07-selfhost}.md`; este documento |
| Runtime | `runtime/sal_runtime.c`, `runtime/sal_runtime.h` |
| Arranque | `src/string_expr.rs` (`string_chain_leaves`, `ChainLeaf`, `accumulates_into`, `expr_is_ref`); `src/ir.rs` (`lower_string_chain`, guarda); `src/llvm.rs` (declaraciones y `*_n`); `src/drops.rs` (`is_accumulate`, `add_is_string_chain`); `src/infer.rs` (`lowering_only_runtime_fn`); `src/methods.rs`, `src/effects.rs`, `src/ownership.rs` (quitar `str_append`) |
| Selfhost | `selfhost/ast/ttypes.sal` (`tchain_leaves`, `tt_is_ref`); `selfhost/ir/fuse.sal` (`lower_str_add`, `str_chain_name`, guarda); `selfhost/emit/emit.sal` (`emit_c_str_add`, guarda); `selfhost/emit/cbuiltin.sal` y `selfhost/emit/cir.sal` (`*_n`); `selfhost/emit/cenv.sal` (prototipos); `selfhost/ir/drops.sal` (`dr_acc`); `selfhost/sem/{infer,effects,ownership}.sal`; `selfhost/ast/tytext.sal` (sin `str_append`; `bytes_to_str` es `String`) |
| Tests | `tests/lang/strings/*` (18 nuevos); `tests/forward_ir.rs`; `tests/run_hello.rs`; `tests/runtime_str_immutable.rs` + `tests/support/sal_str_immutable.c`; `corpus/string_concat_n.sal` (en `tests/selfhost_ir.rs`) |
| Ejemplos, linter y benchmarks | `examples/transformer_gpu.sal`; `.cursor/skills/sal-standard/SKILL.md`; `benches/strings/*`, `benches/strings.sh` |

## Tests

En `tests/lang/strings/`. Fallan con el compilador de `HEAD` (comprobado con un árbol aparte):

| Tests | Fallo con `HEAD` |
|---|---|
| `err_append_removed`, `err_str_append_removed`, `err_append_list_elem_removed` | Compilaban (el último con UAF) |
| `accumulate_self_on_right`, `accumulate_through_ref`, `get_alias_accumulate` | Se caían por UAF o doble free |
| `concat_preserves_operands`, `large_string`, `utf8_concat_slice`, `empty_concat` | Fuga de `+` bajo ASan (`memory_asan`) |

Con fuga bajo ASan en `HEAD`, ya arreglada: `bytes_to_str_independent` (ni `s` ni `b` se soltaban).

De cobertura, que también pasaban antes: `concat_chain_many`, `accumulate_multi_loop`, `clone_independent`, `slice_independent`, `dict_key_unchanged`, `err_borrow_reassign_while_borrowed`, `err_moved_concat_after_bind`.

`tests/lang/methods/method_float_math.sal` pierde su `asan-xfail`: queda limpio en todas las ejecuciones.

Marcas:

| Test | Marca | Motivo |
|---|---|---|
| `dict_key_unchanged` | `# asan-xfail` | Fuga previa del pase heredado: `d.has(k)` con `k` local |
| `err_borrow_reassign_while_borrowed` | `# selfhost-xfail` | El selfhost no ejecuta `check_borrows` |
| `get_alias_accumulate` | `# selfhost-xfail` | El selfhost tipa `get` como `T` |

Otros tests:

- `tests/forward_ir.rs`:
  - `string_plus_chain_lowers_to_one_concat_n`.
  - `string_plus_folds_adjacent_literals`.
  - `string_accumulator_appends_in_place` (ahora con `append_n`).
  - `string_accumulator_through_reference_copies`.
- `tests/run_hello.rs`: el literal no cambia tras `heap = heap + "z"`.
- `tests/runtime_str_immutable.rs`: las primitivas bajo ASan/UBSan, con literales, vacío, solape y tamaño grande.
- Paridad: `corpus/string_concat_n.sal` en `PARITY_FILES` y en `corpus_files()`.

## Selfhost

Espeja la bajada (hojas, plegado, `*_n`, temporales, guarda para parámetros `&`) en los dos caminos a C y en la IR. La IR de `corpus/string_plus.sal` y `corpus/string_concat_n.sal` es idéntica a la del arranque.

No espeja:

- **`add_is_string_chain` por tipo.** `drops.sal` no tiene tipos por expresión, así que un `s = a + b` de variables sigue sin soltarse en un programa que compila el selfhost.
- **La guarda para un local que viene de `get`.** El selfhost tipa `get` como `T` y el acumulador puede escribir en el elemento de la lista. Es el hueco que marca `get_alias_accumulate`.

## Medidas

Pendiente de la fase 5 (`BENCH_LABEL=antes` / `despues` en `benches/strings.sh`).
