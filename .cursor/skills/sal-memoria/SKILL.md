---
name: sal-memoria
description: >-
  Guía la liberación de locales en sal: el pase insert_drops, la cola de
  campos de un struct, el espejo en el selfhost y la invariante de no liberar
  dos veces ni usar tras liberar. Usar al editar src/drops.rs, la cola de
  structs en src/ir.rs, runtime/sal_runtime.c, selfhost/ir/drops.sal, la cola en
  selfhost/ir/fuse.sal, emit.sal (cola C), tests/drops.rs, tests/lang/drops, o al
  hablar de free, list_free, dict_free, enum_free, bytes_free, tensor_free,
  fugas, LEAK, uso tras liberar o doble liberación, struct_copy, list_get de
  struct, struct_free_box o caja de struct.
---

# Memoria

## Modelo (el que manda)

La memoria de sal es la de Rust, con `@arena` como única extensión (SPEC «Memoria»; subplan [13-ownership-rust](../../../docs/subplans/13-ownership-rust.md)):

- Un valor con dueño se **mueve** por defecto. Usarlo después es `E_MOVED`.
- `&T` y `&mut T` **prestan**, con lifetimes y la elisión de Rust. Un préstamo que se solapa, o una referencia que sobrevive a su referente, es `E_BORROW`.
- La copia profunda es **`x.clone()`** explícito. No hay copias implícitas, ni modos `borrow`/`take` inferidos.
- Cada valor se libera al salir de su ámbito con la **glue de su tipo** (`drop.<Tipo>`). Una lista de structs con glue se crea con el glue de su elemento (`sal_list_set_drop`).
- `@arena` es el único mecanismo añadido: el asignador por región y el descarte de páginas al salir.

Lo que sigue de esta sección, a partir de «Dos sitios», describe el estado del arranque antes de cerrar el subplan 13. Las heurísticas que aparecen ahí (`fresh_kind`, `tainted`, `strings_moved`, `borrow`/`take` inferido) son legado a sustituir, no reglas a extender.

La autoridad es [SPEC.md](../../../SPEC.md) («Liberación de locales»). Resumen operativo en [docs/subplans/03-semantics.md](../../../docs/subplans/03-semantics.md) y [04-ir-runtime.md](../../../docs/subplans/04-ir-runtime.md). Si la SPEC y el código divergen, no cierres una a ojo: avisa y ciérralas en el mismo cambio.

Invariante: una fuga es aceptable; una doble liberación o un uso tras liberar no. Ante la duda, no sueltes.

Dentro de una función `@arena`, el `free` de un puntero interior es un no-op y `sal_arena_leave` descarta las páginas al salir, después de esos `free`. Eso no sustituye la cola de drops fuera de `@arena`: un bloque del `malloc` del sistema se libera de verdad. Un `leave` que no llega (un pánico) puede fugar las páginas; no puede dejar un puntero vivo hacia ellas.

## Dos sitios, los dos compiladores (estado actual, legado)

| Qué | Arranque | Selfhost |
|-----|----------|----------|
| Locales (`free`, `list_free`, `dict_free`, `enum_free`, `bytes_free`, `tensor_free`, `struct_free`, `struct_free_box`), antes de bajar | `src/drops.rs` (`insert_drops`) | `selfhost/ir/drops.sal` (`drops_body_t`) |
| Cola de campos de un struct (IR) | `src/ir.rs` (`free_struct_strings`, `field_reads`, `emit_struct_string_drops`, `field_drop`) | `selfhost/ir/frc.sal` (`FRC` y sus métodos, `drop_fields`, `fr_*`), `selfhost/ir/desc.sal` (`drop_field_fn`), `selfhost/ir/fuse.sal` (`emit_struct_drops`) |
| Caja de la copia que toma posesión (`sal_desc_copy` de `S…`, `sal_list_pop`) tras fusión | `src/ir.rs` (`finish_module_box_shell_drops`), invocado desde `src/fuse.rs` | el arranque en `emit ir` / tests; ver nota selfhost abajo |
| Cola en C (stage2) | — | `selfhost/emit/emit.sal` (`emit_c_struct_scope_frees`, importa `frc` y `desc`) |
| Movimiento parcial y `borrow`/`take` | `src/ownership.rs` | `selfhost/sem/ownership.sal` |
| Runtime (`sal_dict_*`, `sal_enum_*`, vistas `bytes`, `sal_tensor_free`) | `runtime/sal_runtime.c` | el mismo |

`insert_drops` va entre `TypedProgram` y `lower_program`. No compruebes en un pase posterior lo que este ya decide. El selfhost arma `drop_env_t` en `fuse.sal` (`enum:`, `struct:`, `desc:`, `ownfn:`, `retk:` para la función que devuelve un contenedor, `glue:` para el struct sin descriptor) para `dr_*`.

Un cambio de esta zona toca los dos compiladores en el mismo cambio (y runtime si cambian kinds o dueño). La IR tiene que coincidir; el emisor C del selfhost debe espejar la cola cuando stage2 ejecuta.

## Qué suelta `insert_drops`

Un local se libera si se liga una sola vez a un valor **fresco** (`fresh_kind`) y no escapa:

- **`String`**: literal, builtins de cadena fresca, o `+` con operando `String`.
- **`List` / `Dict`**: tipos y dueño según SPEC. Lo guardado se movió al contenedor: la marca tainted solo cuenta en el arranque cuando no hay movimientos publicados (`facts.moved`), y el selfhost no la usa. También son frescos el contenedor que devuelve una función de usuario (`user_call_kind`, `retk:`), `xs.clone()` / `d.clone()` / `b.clone()`, la lista de listas y `List.new[S]()` de un struct con glue (`list_glue_arg`, `glue:`).
- **Reasignación**: un local ligado más de una vez, siempre a un valor fresco del mismo tipo (`rebound_locals`, `dr_rebound`), suelta el valor viejo al reasignar (`rebind`, `dr_rebind`): `__new<N>` = valor nuevo, suelta el viejo, asigna, repone `__live`.
- **`x.clone()`** solo lee su receptor (`borrows_args`, `dr_borrows`). El clon de un struct describible es una caja `struct:<desc>` (`struct_copy_kind`; en el selfhost `dx_clone_desc`, que solo conoce el struct de un parámetro o de un `list_get`); el de un struct con `clone.<Struct>` lo suelta la cola de structs (`value_struct` en `src/ir.rs`; alias en `FRC.alias_call` si es describible).
- **`Dict`**: valores escalares, `String`, enum sin type params o struct describible (kinds en `sal_dict_new`; ver `dict_val_kind` en `src/ir.rs`).
- **Enum** (caja de variante), **`Bytes`** (`bytes_new`; **`bytes_view`** / **`bytes_view_mem`**: solo cabecera de vista).
- **`dict_get`**: `Option` fresco → `enum_free` si no escapa; `match dict_get(…)` descartado como sentencia igual.
- **Literal `tensor`**: `tensor_free` → `sal_tensor_free` / `sal_place_free` en programa escalar.
- **`OWNED_FNS`**: retorno `String` siempre fresco; **`if`/`match` de nivel superior** vale si **cada** rama es fresca; un **`while` de nivel superior** excluye la función.

Los argumentos movidos a un **constructor de struct** (`hard_move` en el análisis de escapes) no se liberan otra vez como local en ese ámbito.

Otras reglas:

- **Descarte** (`discard_kind`): `xs.pop()`, `get`, `int_to_str(n)`, `match dict_get(…)` → temporal `__dis<N>` y suelta en la sentencia (`dr_next_tmp_p` en selfhost).
- **Solo lectura** (`read_only_params`): punto fijo optimista.
- **`__live_x`**: escape solo en algunas ramas del mismo nivel de bucle.
- **`list_push` / `list_set` / `dict_put`**: prestados como sentencia o `_nombre`; `ys = xs.push(v)` escapa. Su receptor no cuenta como movido para la cola de structs (`field_reads`, `fr_call`): `c.items.push(v)` deja `items` con su struct.
- **Cola que es una llamada** (`is_call_tail`, `dr_is_call_tail`): mueve sus argumentos detrás de su bandera, como una sentencia.

### Préstamo de `list_get` y copia al tomar posesión

`list_get` de `String`, struct describible o enum **presta** el puntero guardado. No llama a `desc_copy` y el local no se libera: lo suelta la lista (`list_free` / `list_pop`). Pasarlo a una función que solo lee campos, o que los recompone (`P(p.name, p.n)`), sigue prestando: el llamador no clona. El resultado es un alias (los mismos punteros) y la cola no suelta sus hijos. `p.x = v` sobre ese elemento escribe el hueco (`sal_struct_set`); no reserva otro bloque.

La copia profunda (`desc_copy`, sitio `struct_copy`) queda solo para una segunda dueña que libera por su cuenta: `return` cuyo destino vive más que la lista, el parámetro mismo guardado o devuelto (`list_push(ys, p)`, `return p`), `list_push` / `list_set` / `dict_put`, y el payload de una variante. Un constructor armado solo con campos prestados no copia. El destino posee esa copia y se libera una vez. `list_pop` sigue moviendo (el elemento sale de la lista). Un `desc_copy` guardado en una lista es un valor fresco: la lista no queda tainted y `list_free` suelta la copia.

| Destino | Qué pasa |
|---------|----------|
| `list_push`, `list_set`, `dict_put` del alias | Un `sal_desc_copy`. La lista absorbe la copia. No `struct_free_box` |
| Función que guarda o devuelve el parámetro mismo | `sal_desc_copy` en la llamada y, tras escalarizar, `sal_struct_free_box` de esa caja |
| Lectura de campos, o recomponerlos | No hay copia ni `struct_free_box`. El resultado es un alias |

En **`src/ir.rs`**, tras **`fuse_module`**: `scan_struct_box_shell` registra destinos de `sal_desc_copy` cuyo descriptor empieza por `S` y de `sal_list_pop` (solo si `loop_depth == 0`). No registra `sal_list_get`. Marca el shell al pasar la copia a una llamada no-`sal_` que no transfiere caja, e inserta `sal_struct_free_box` **inmediatamente antes** del último `Return`. Una copia devuelta la suelta `scalarize` después de extraer los campos al `sret`, no este pase (soltarla antes sería un uso tras liberar).

Runtime: **`sal_list_get_typed`** devuelve el elemento tal cual. **`sal_desc_copy`** es la copia explícita. **`sal_struct_free_box`** libera solo el bloque; **`sal_struct_free(desc)`** recorre campos y la caja.

Al **compilar módulos bajo `selfhost/`**, `src/compile.rs` llama `fuse_module_opts(..., box_shell_drops: false)` para no aplicar ese pase IR al código del autohospedador (mismo motivo de ámbito/SSA en bucles del compilador). `sal emit ir` y tests del arranque siguen usando `fuse_module` con el pase activo.

## Cola de un struct

La emite `lower_fn` (y el C de `emit.sal`) después del cuerpo y antes del `return`. Un struct local o parámetro `take` suelta:

- `String` (`sal_free`), enum (`sal_enum_free`), `Bytes`, `List`/`Dict` de escalares Copy.
- **`List[String]`** y **`Dict[..., String]`** si **`container_tainted`** no marca el campo (solo valores frescos guardados).
- **`List[P]`** con **`P`** struct describible (misma regla de tainted/glue que `List[String]`): `sal_list_free` del campo cuando el struct local no se movió entero.
- Structs anidados, salvo lectura del struct entero.

`field_reads` recorta la cola:

| Conjunto | Efecto |
|----------|--------|
| `strings_moved` | Campo `String` o **enum** entregado por valor (`return n.name`, arg que lo conserva): no `sal_free` / `sal_enum_free` en esa ruta. |
| `heap_moved` | Salió un contenedor `List`/`Dict`/`Bytes` entero: no suelta contenedores en ese local. |
| `struct_valued` | Se leyó un struct anidado entero (`q = w.p`): no desciende. |
| `container_tainted` | `push`/`put` con valor no fresco en ese campo: no `list_free`/`dict_free` del contenedor. |

## Runtime (dueño explícito)

- **`sal_dict_get`**: construye `Some`; si `V` es `String`, cabecera del `Some` marca dueño del campo copiado; enum/struct se copian en profundidad.

## Autohospedaje (`selfhost/ast/tast.sal`)

- **`tp_free_*`**: `seen` deduplica cajas por `enum_box_id` (una variante unitaria da 0 y no hay nada que soltar). `lseen` deduplica listas por `list_box_id` (`stmts` de un bloque, argumentos de llamada, `elsifs`, `arms`), porque el desazucarado comparte listas entre bloques (`lp_unit` devuelve `TBlock(b.stmts, XNone)`).
- Descriptores: `List[TStmt]` es `S(isEsiEiii)` (`+` si es dueña). `list_get` presta el elemento; la copia (`sal_desc_copy` / `sal_enum_copy`) ocurre solo al tomar posesión. `List[TFn]`, `List[TProgram]`, `List[TStruct]`, `List[TEnum]`, `List[TArm]` y `List[TElsif]` son `i`: `list_get` devuelve el mismo valor, y `list_set`/`list_free` no sueltan el elemento.
- **`sal_bytes_view`**: flag `view`; `sal_bytes_free` no libera el `data` ajeno.
- **`sal_dict_free` / `dict_put`**: `dict_val_release` para `String`, enum (kind 5) y struct empaquetado (kind `4|…`).

## Cola de struct: vista de los llamadores

- Un argumento struct pasado a un parámetro de modo inferido que el llamado consume cuenta como movido para la cola del llamador, aunque el modo inferido sea `borrow`. Consumir es devolverlo, guardarlo o pasárselo a quien lo consume. Un campo Copy (`return p.n`) escapa para drops (el llamado no lo suelta) pero no consume: si contara, la cola dejaría de soltar el `String` que nadie más suelta. `field_reads` usa esa misma vista. Un parámetro que el cuerpo pasa suelto a un builtin que libera (`list_free(p)`) también es `take` para los llamadores.
- Arranque: `caller_param_modes` en `src/ir.rs`, a partir de `drops::last_consumed_params`. El cálculo incluye las funciones de los otros módulos del grafo, así un consumidor importado (`tp_free_shallow`) también cuenta. Selfhost: `DC.took` en `drops.sal` (un campo Copy escapa y no entra; el flag vive en la lista `copy_ok`, alineada con `Type.field`) y `mark_escaping_params` en `modes_fill_t` (`fuse.sal`), que promueve solo lo consumido. Los flags de cada función van en un vector por índice (`dr_cons_list`), no en un mapa por nombre. Un cuerpo cuyo valor es el parámetro (`pin_tail_take`, y `own_pin_tail` en ownership) es `take` para los llamadores. Tests: `struct_glue_*` en `tests/drops.rs`, `tests/glue_escape/` en `PARITY_FILES`, `corpus/param_modes.sal` y `tests/lang/drops/drop_struct_freed_by_callee_of_callee.sal`.

## Siguen sin liberarse (SPEC)

Un struct multi-campo suelta su bloque en `drop.<Struct>` (`sal_struct_free_box`); un valor solo en registros (scalarize) no tiene caja que soltar. Siguen abiertos: el payload `List`/struct de una caja de enum copiada con `sal_enum_copy` (se comparte); campos de caja enum **no frescos**; **`list_pop`** de lista no dueña; **`String` como asa `Int`**; **`z = k`** sobre la misma copia. Tensores en región fusionada: `IrInst::Drop` + `sal_place_free` en bajada LLVM, no el mismo camino que un local escalar.

## Verificar

| Se tocó | Comando |
|---------|---------|
| `src/drops.rs`, `src/ir.rs`, `src/fuse.rs`, `tests/drops.rs` | `cargo test --test drops` |
| `tests/lang/drops/` (p. ej. `drop_list_struct_box_pass_fn.sal`, `drop_struct_list_field.sal`) | `cargo test --test lang` (`# exit:` = retorno de `main`, no “OK = 0”; ver [sal-conformidad](../sal-conformidad/SKILL.md)) |
| Selfhost drops/fuse/emit | `cargo run -- parity <fichero>`; `cargo test --test selfhost_ir -- --test-threads=1` (paridad IR puede diferir en programas con caja si solo el arranque aplica el pase IR) |
| Runtime | `tests/instrument_sal.rs` (lectura sin `struct_copy`; la copia que escapa sin `LEAK` ni `DOUBLE_FREE`) + `tests/drops.rs` |

`SAL_INSTRUMENT_STRICT=1` tras `sal build --instrument`: stderr sin `LEAK` ni `struct_copy` residual en programas que el compilador debe cerrar (`USE_AFTER_FREE`, `DOUBLE_FREE` siguen siendo fallo duro).

Un test que sigue pasando con el código viejo no cuenta. Quita `# xfail:` / `# selfhost-xfail:` al cerrar un hueco en el mismo cambio.
