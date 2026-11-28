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
  struct, struct_free_box o caja de struct, Hoist, temporales, asan-xfail o
  --sanitize.
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
- **`dict_get`**: `Option` fresco → `enum_free` si no escapa; `match dict_get(…)` descartado como sentencia igual. El `Hoist` también liga a un local el scrutinee `match dict_get(…)` (`is_dict_get`), como el de `list_get`/`list_pop`. Con payload `Int`/`Float`/`Bool`, un `Some(v) => v` no cuenta como escape (ver «Escalares» abajo). El `Some` es dueño de la copia: `String` (bit 1), enum (bit 2) y, para un struct describible de dos o más campos, la caja con sus `String` (bit 3 y hueco `clone` = `-(nfields | strings << 8)`, que `sal_enum_release_field` y `sal_enum_copy` entienden; `collections/dict_struct_string_get.sal`).
- **Literal `tensor`**: `tensor_free` → `sal_tensor_free` / `sal_place_free` en programa escalar.
- **`OWNED_FNS`**: retorno `String` siempre fresco; **`if`/`match` de nivel superior** vale si **cada** rama es fresca; todo `return` anidado (en un `while`, un `if` o un brazo) tiene que serlo; un local fresco ligado una vez arriba sigue siéndolo si en los bloques anidados solo recibe valores frescos (el acumulador `out = out + c` del bucle). Una llamada a otra función dueña también es fresca: `owned_return_fns` itera hasta el punto fijo (selfhost: `owned_fns_fix`). Tests: `tests/lang/drops/drop_string_fn_{loop_result,nested_return,imported,tail_call}.sal`.
- **Payload movido por un brazo** (`take_moved_payloads` / `dr_take_payloads`): en `match t { V(s) => s }` con `t` un enum propio (parámetro por valor, o local ligado una vez a un constructor o a una función que construye el enum), el brazo pasa a `enum_take(t, i)`, que devuelve el campo y borra su bit de propiedad en la caja; `t` se suelta al final con lo que le queda y el que liga el `match` es dueño del payload (`fresh_kind` de un `Match`). Un campo genérico solo si el tipo de `t` viene de una firma. Selfhost: la clase de suelta va en `targs` de la llamada; claves `vfk:`, `retak:`. Si el brazo mueve la ligadura a otro sitio (`u = s` a cualquier profundidad, argumento de una llamada que no está en `borrows_args`/`dr_borrows`, o cola del bloque del brazo), el brazo empieza con `s = enum_take(t, i)` y el patrón deja ese campo en `_` (`take_moved_binder` / `dr_tk_moved_binder`): `s` es un local del brazo y se suelta o se mueve como cualquiera. Sin esto la ligadura que escapa marcaba `t` como escapado y se perdían los dos. Un local ligado una vez a un valor fresco dentro de un bloque y entregado como su cola cuenta como retorno con dueño (`owned_block_string` / `owned_block_ok`). Tests: `tests/lang/drops/drop_match_binder_moved{,_tail}.sal`.
- **Temporales que no eran del llamante** (cerrados al pasar `sal test --sanitize tests/sal`): `b.ptr()` y `bytes_view(b, ..)` solo leen `b` (`borrows_args` / `dr_borrows`; antes `buf.ptr()` movía `buf` y no se soltaba). Un `Bytes` nuevo leído una vez (`Bytes.view_mem(p, n).to_str()`, `mk(4).len()`) se liga a un temporal (`is_fresh_bytes` + `reads_container_arg`; `dr_any_temp_call` + `dr_reads_container_arg`). Un struct con glue que entrega una fn o su constructor y se presta a `&self` (`Path.new(s).parent()`, `Response(raw).body()`) se liga a `__tmp_<fn>_<n>` con su tipo en `TMP_STRUCTS` (`user_struct_temp`; `dr_user_struct_temp`). Los temporales de una condición `elsif` se ligan dentro de ella, en un bloque con ámbito propio, como el operando derecho de `&&`. Tests: `drop_bytes_ptr_keeps_owner`, `drop_bytes_temp_reader`, `drop_struct_temp_lent_self`, `drop_elsif_cond_temporaries` en `tests/lang/drops/`. Después, con el mismo barrido: (1) las fns `-> String` del llamante son el punto fijo máximo (recursivas como `path_parent_i`), posible porque un brazo de `match` que da `&String` donde otro da `String` es `E_TYPE` (antes el arranque solo miraba el primer brazo, y el selfhost devolvía así `flags` en `ar_expr` y `e` en `fr_innermost`); (2) una fn de otro módulo que hace `s = s + g(i)` se lee con la firma de `g` (`STRING_RET_FNS`, `retstr:`), no con la tabla de tipos del que llama (`bytes_to_hex`); (3) `invalid().ss.len()` liga el struct a un temporal (`kem_invalid_result` de ML-KEM). `std/io/net.sal`: `sockaddr_*` devolvían el puntero de un `Bytes` local ya liberado; ahora devuelven el `Bytes`. Tests: `drop_string_recursive_owned`, `drop_string_concat_imported`, `drop_struct_temp_field_read`, `types/err_match_arm_ref_string`.
- **Movimiento parcial** (`n = p.name` con `p` struct propio): `moved_field_kind` da al local la suelta del campo y la cola del struct se salta ese camino (`field_reads` marca `strings_moved`, también para `List`/`Dict`/`Bytes`). Selfhost: `dr_pm_owned`, claves `sname:`, `fk:`, `fst:`, `rets:`; `fr_mark_heap` en `frc.sal`.

Los argumentos movidos a un **constructor de struct** (`hard_move` en el análisis de escapes) no se liberan otra vez como local en ese ámbito.

Otras reglas:

- **Descarte** (`discard_kind`): `xs.pop()`, `get`, `int_to_str(n)`, `match dict_get(…)` → temporal `__dis<N>` y suelta en la sentencia (`dr_next_tmp_p` en selfhost). `xs.pop()` de una lista que la fn no posee (un `&mut List` parámetro) pasa a `list_pop_drop` (`pop_drop` / `dr_pop_drop`): `sal_list_pop_drop` suelta el elemento si la lista posee los suyos, como haría `list_free` (`drops/drop_list_pop_discard_param.sal`).
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

## `Hoist`: temporales que alguien tiene que soltar

`Hoist::expr` (`src/drops.rs`; espejo `dr_hoist_*` en `selfhost/ir/drops.sal`) liga a un local `__tmpN`, antes de la sentencia, lo que no tiene dueño, para que el fin de ámbito lo suelte (Rust: temporal de la sentencia):

- Un temporal fresco (`is_fresh_temp`: `d"…"`, `int_to_str`, `x + y`, llamada que devuelve `String` dueño) pasado a un builtin de solo lectura (`reads_string_arg` / `dr_reads_string_arg`). **`strdup` está en esa lista**: `d"ab".dup()` es `strdup(strdup("ab"))` y el interno se libera. `strdup` también está en `borrows_args` / `dr_borrows`; si no, el análisis lo toma por consumido y el flag de movido impide liberar.
- Lo que no es un lugar bajo `&` / `&mut` (`f(&g())`).
- El scrutinee `list_get`/`list_pop`/`dict_get` de un `match`, y la base de `list_get(..).f`.
- **La primera condición de un `if`** (corre una vez, antes de la sentencia). No las de `elsif` ni la de `while`: se reevalúan o son condicionales. Por lo mismo, **los temporales del operando derecho de `&&` / `||` no salen de él**: ligarlos antes de la sentencia los evaluaría aunque la izquierda lo impida (`i < n && f(&xs.get(i))` leía fuera de rango). Se ligan en un bloque que acaba con el operando (`dr_hoist_cond_operand`), y `xform_logic` / `dr_xform_logic` le da ámbito, también en la condición de un `if` y el escrutinio de un `match`, así que se sueltan dentro (`tests/lang/control_flow/and_right_operand_after_guard.sal`).
- **Los temporales de un brazo de `match`** se ligan dentro del brazo (`Hoist::arm_body`, `dr_hoist_match_arms`), nunca antes del `switch`, donde las ligaduras del patrón aún no existen. Un brazo bloque se recorre como bloque; un brazo expresión pasa a bloque solo si liga algo (`tests/lang/drops/drop_match_arm_temporaries.sal`). En el selfhost, `v + w` con `v` ligado por el patrón solo se reconoce como cadena si `stradd.sal` tipa `v`: lo hace con el tipo del campo, en un `match` por valor o por referencia (`&Campo`), de un enum genérico y bajo una variante anidada (`sa_arm_pattern`; `tests/lang/drops/drop_match_binder_string_concat.sal`).

Espejo: `dr_hoist_expr` devuelve una **copia** (`dr_alias` = `tc_expr`), así que `dr_moved` siempre ve una caja distinta. Sustituye un hijo solo si el hoist añadió temporales (`list_len(out)` creció, como en `dr_hoist_if`); reemplazarlo siempre cambia la identidad de caja de la que dependen otras tablas.

## Escalares

Una lectura de un `Int`/`Float`/`Bool` no alias nada del heap: no marca `escaped`, `consumed` ni `kept` (`expr_is_copy_scalar`, en `expr` y en `arg` de `src/drops.rs`). Sin esto, `Some(v) => v` sobre `Option[Int]` impedía liberar la caja del `Option`. El selfhost no tiene tipos por expresión en `drops.sal` y la espeja por casos: `dict_get` sobre un `dict_new[K, Int|Float|Bool]` (`dr_note_copy_payload`) y el resultado de una fn de usuario (`retsc:`, `dr_note_local_type`). `tests/lang/drops/drop_dict_get_int_payload_*.sal` dan la misma IR en los dos. Un `Option[Int]` que no llega por esos caminos sigue contando como escape en el selfhost.

## `str_as_int(&s)`

Guarda el asa de `s`: `s` escapa y no se libera, **sin flag**. En el arranque es un caso aparte en `expr` (`escaped` + `soft.remove`). En el selfhost es `mov == 4` en `dr_call_args` y la rama `mov == 4` de `dr_arg_escape` (`c.take`, `soft = 0`, `dr_escape`); con `mov == 2` (constructor) salía un flag muerto `if flag == 1 free(..)`.

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

Huecos medidos con ASan (`# asan-xfail:`; inventario y causas en [sal-memoria-asan](../sal-memoria-asan/SKILL.md)): `String` por valor a una función genérica; `int_to_str` como clave de `Dict`; `f64_bits_to_str`. Los locales del bloque de un brazo de `match` sí se sueltan (con un brazo bloque, cada brazo es una rama: `walk_match_top`); los struct de un cuerpo de `while` se sueltan al final de cada vuelta, como los de una rama de `if` (`block_struct_drops` + `OPEN_BRANCHES` en `src/ir.rs`, `lower_loop_body` en selfhost; `drops/drop_struct_loop_local.sal`), y un struct movido solo en una rama se suelta tras su bandera en las demás (`branch_moved`; ver subplan 13).

Un struct multi-campo suelta su bloque en `drop.<Struct>` (`sal_struct_free_box`); un valor solo en registros (scalarize) no tiene caja que soltar. Siguen abiertos: el payload `List`/struct de una caja de enum copiada con `sal_enum_copy` (se comparte); campos de caja enum **no frescos**; **`list_pop`** de lista no dueña ligado a un local; **`String` como asa `Int`**; **`z = k`** sobre la misma copia. Tensores en región fusionada: `IrInst::Drop` + `sal_place_free` en bajada LLVM, no el mismo camino que un local escalar. Los búferes que reserva la bajada LLVM (`t + t`, `-t`, `transpose`, salida de `matmul`) se sueltan al final de `main` (`free_main_buffers`, `emit_host_tensor_fn`) con y sin `--instrument`; con `--instrument` salen de `sal_instrument_malloc` y `sal_place_free` los devuelve a él. Dueños de tensor en drops (los dos compiladores, misma IR):
- Una fn de usuario que devuelve `Tensor` entrega el búfer al llamador (`user_call_kind` → `tensor_free`; selfhost `rett:<fn>` → kind 10), ligado o descartado. Reasignar un local tensor (`_ = f(..)` dos veces) suelta el viejo (`rebound_locals` / `dr_plain_kind` admiten `tensor_free`).
- `y = to p e` posee el tensor si `e` es un literal, un resultado tensor de fn de usuario, otro `to` así, o un local vivo con `tensor_free` (que `to` mueve): `to_tensor_kind` / `dr_to_tensor_kind`. Un `load` o un parámetro no cuentan.
- El constructor de un `frame` (item `frame` o struct con campo `Tensor`) lee sus columnas, no las mueve (SPEC «Memoria»): `is_frame_ctor` en drops y en `src/borrowck.rs` (si no, `published_moved_locals` da la columna por movida y nadie la suelta); selfhost `frame:<F>` en `flat_map_structs_t`.
- LLVM: una fn de usuario con tensores (`emit_general_fn`) suelta antes del `ret` los búferes de nivel superior (`TensorBin`/`TensorUnary`/`PlaceCopy`) que no devuelve, no suelta el IR ni pasa a una llamada (`free_fn_temporaries`). En `emit_host_tensor_fn` un `sal_tensor_free` del IR sobre un búfer de `owned` lo saca de `owned` (como `Drop`), y `sal_instrument_shutdown` solo va al final de `main`.
- Un parámetro tensor por valor es del que recibe: lo suelta al salir salvo que lo mueva (`owned_param_release` → `tensor_free`; selfhost `dr_param_generic_kind` → 10). El emisor del que llama lo saca de su `owned` (`release_moved_tensor_args`, por `FnSig.tensor_by_value`), o lo soltaría otra vez al salir.
- `to p x` de tamaño que el compilador no conoce (un `load`, un tensor que devuelve una fn) mueve de verdad: `sal_place_move` copia al lugar destino por el tamaño que guardó su heap (`SalPlaceBlock.size`; con `--instrument`, el bloque instrumentado) y suelta el origen; el destino queda solo en `owned` (`take_moved_tensor`). Un bloque que ya está en el destino, o que ningún heap conoce (región arena), se queda. `load` reserva en el heap de cpu (`sal_place_malloc`).
- `transpose(t)` mueve `t` (`is_move_fn` en `src/ownership.rs`) y el emisor copia: suelta la entrada si es de la fn (`fn_owns_buffer`: `owned`, parámetro por valor o `pret_`), nunca la de un `&T`. `reduce` lee (`borrows_args`/`dr_tensor_reads`), y la ruta de `load` es un argumento leído (`reads_string_arg`/`dr_reads_string_arg`).
- Una op elemento a elemento (`relu`, `silu`, `rms_norm`, `transpose`, `reduce`, `attention`, `qmatmul`) reserva su resultado en el lugar de su entrada (`buf_place`, `place_of_arg`), no siempre en cpu.

Una fn genérica en el lugar (`on p`) se emite una vez y lee al entrar el lugar de su primer argumento en `p` (`sal_place_of`, `Pl::Dyn`); un eje `?` de un parámetro tensor es el tamaño del búfer recibido entre el resto de ejes (`sal_place_size`, `FnSig.tensor_params`), y `relu`/`silu`/`rms_norm` aceptan esa longitud en tiempo de ejecución. El resultado que una fn devuelve desde su región fusionada se copia al heap del lugar antes de cerrarla (`sal_region_escape`; la región guarda el tamaño de cada bloque).

Abierto: en el que llama, un tensor con eje `?` (un `load[...?...]`, `to cpu f(..)`) baja a IR con longitud 1 y el lugar del tipo de retorno de `f` (`p`), así que `-y` o `y + 1.0` sobre él leen un elemento. `c = on cpu` seguido de un bloque no registra `c` como tensor en la bajada: `c - k` sale como escalar menos tensor. `softmax` y `reshape` devuelven el mismo búfer que leen. LSan no ve fugas de tensor: `g_place_heaps` mantiene el bloque alcanzable; se miden con `--instrument` y `SAL_INSTRUMENT_STRICT=1` (`tests/instrument_sal.rs`). Un bloque vigilado y no reservado por la instrumentación vuelve a malloc al soltarse: al reutilizarse, `purge_reused_locked` borra su entrada de cuarentena (si no, falso `USE_AFTER_FREE`).

## Verificar

| Se tocó | Comando |
|---------|---------|
| `src/drops.rs`, `src/ir.rs`, `src/fuse.rs`, `tests/drops.rs` | `cargo test --test drops` |
| `tests/lang/drops/` (p. ej. `drop_list_struct_box_pass_fn.sal`, `drop_struct_list_field.sal`) | `cargo test --test lang` (`# exit:` = retorno de `main`, no “OK = 0”; ver [sal-conformidad](../sal-conformidad/SKILL.md)) |
| Selfhost drops/fuse/emit | `cargo run -- parity <fichero>`; `cargo test --test selfhost_ir -- --test-threads=1` (paridad IR puede diferir en programas con caja si solo el arranque aplica el pase IR) |
| Fugas, doble free, uso tras liberar (oráculo independiente) | `cargo test --test memory_asan -- --test-threads=1`; ver [sal-memoria-asan](../sal-memoria-asan/SKILL.md) |
| Runtime | `tests/instrument_sal.rs` (lectura sin `struct_copy`; la copia que escapa sin `LEAK` ni `DOUBLE_FREE`) + `tests/drops.rs` |

`SAL_INSTRUMENT_STRICT=1` tras `sal build --instrument`: stderr sin `LEAK` ni `struct_copy` residual en programas que el compilador debe cerrar (`USE_AFTER_FREE`, `DOUBLE_FREE` siguen siendo fallo duro).

Un test que sigue pasando con el código viejo no cuenta. Quita `# xfail:` / `# selfhost-xfail:` al cerrar un hueco en el mismo cambio.

## Oráculo independiente: ASan/LSan/UBSan

`sal --sanitize` y `cargo test --test memory_asan -- --test-threads=1` barren `tests/lang/{drops,ownership,collections,structs,enums,strings,methods,generics}` y `tests/memory/` y exigen cero informes. Un hueco de liberación conocido lleva `# asan-xfail: motivo`; al cerrarlo, **quita la marca en el mismo cambio**. Un cambio de memoria no está hecho hasta que pasa. Flujo completo, inventario de huecos y cómo triar un informe: [sal-memoria-asan](../sal-memoria-asan/SKILL.md).
