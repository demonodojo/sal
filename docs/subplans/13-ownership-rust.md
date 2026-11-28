# Subplan: ownership y liberación al estilo Rust

La autoridad de la superficie es [SPEC.md](../../SPEC.md) («Memoria», «Liberación», «Funciones @arena», gramática de `type`, `unary` y `method_params`). Este subplan fija archivos, algoritmo y tests. Si diverge de la SPEC, manda la SPEC.

`@arena` no cambia de mecanismo: sigue siendo el asignador por región. El ownership dice quién es el dueño; el arena dice de qué páginas sale el bloque.

## Qué cambia

| Antes | Ahora |
|-------|--------|
| Préstamo solo en el argumento; modo `borrow`/`take` inferido | `&T` y `&mut T` son tipos. `T` mueve, `&T` presta, `&mut T` presta en exclusivo |
| Lifetimes no existen | `'a` en firmas y structs, con la elisión de Rust |
| Se suelta solo un local «fresco» que no escapa | Se suelta todo valor con dueño al acabar su ámbito, salvo que se haya movido |
| Copia profunda implícita al tomar un `list_get` | `x.clone()` explícito; sin él, `E_BORROW` |
| Un bloque por elemento de `List[Struct]` | Los campos de un struct describible van en el bloque de la lista (`S(..)`), con `get` prestando un puntero interior y `push`/`pop` copiando slots |
| `borrow` / `take` / `keep` | `E_PARSE`. `keep` es `&mut List`/`&mut Dict` de una función `@arena` |

`Copy` sigue implícito: `Int`, `Float`, `Bool`, `Unit` y un struct cuyos campos lo son. `&T` compartida es `Copy`. `&mut T` no. No hay `mut` en los bindings. `panic` aborta: la fuga de un pánico sigue aceptada.

## Gramática y árbol

- `Type::Ref { mutable, lifetime, inner, span }`. `lifetime` es `Some("a")` o `None` antes de la elisión.
- `UnOp::Ref`, `UnOp::RefMut`, `UnOp::Deref`.
- `FnDef.self_ref`: `0` por valor, `1` `&self`, `2` `&mut self`. El desazucarado inserta `self` con ese tipo.
- Un lvalue es un `IDENT`, una cadena de campos o un `*` (asignación a través de `&mut`).
- `schema_version` 6.

## Elisión

Sobre cada `fn` / `method`, antes de tipar el cuerpo (`src/infer.rs`, `elide_fn_lifetimes`):

1. Cada lifetime elidido en posición de entrada recibe uno distinto (`'e0`, `'e1`, …).
2. Si el retorno elide un lifetime y hay exactamente un lifetime de entrada, usa ese.
3. Si el receptor es `&self` o `&mut self`, el retorno elidido usa el lifetime de `self` (gana a la regla 2).
4. Si no aplica ninguna, `E_BORROW`.

En un campo de `struct` el lifetime es explícito. Un `'a` que no está en los parámetros de tipo es `E_TYPE`.

## Borrow checker

`src/borrowck.rs` (`check_borrows`), espejo `selfhost/sem/borrowck.sal`. Sustituye a `check_ownership`. Publica en el programa los puntos de drop que consume `elaborate_drops`.

El control de sal es estructurado (`if`, `while`, `match`, `return`, `break` ya reescrito a bandera). No hay un CFG aparte: el punto fijo recorre el `while`.

**Movimiento.** Un lugar es `nombre` o `nombre.campo.….`. Se mueve al ligarlo, devolverlo, pasarlo por valor, guardarlo (`push`/`set`/`put`) o usarlo en un constructor. Usarlo después es `E_MOVED`. Mover un campo marca ese campo; el resto sigue usable y `p.campo = v` lo repone. Un valor vivo en el arco de vuelta de un `while` que el cuerpo mueve es `E_MOVED` (la segunda vuelta). `Copy` y `&T` compartida no se mueven. Sacar un no-`Copy` de `&T` o `&mut T` (`*r`) es `E_BORROW`.

**Préstamos (NLL acotado al ámbito).** `&p` / `&mut p` crean un préstamo del lugar ligado al local que guarda la referencia (un temporal de sentencia si no se liga). El préstamo vive desde su creación hasta el último uso de ese local dentro de su ámbito. Si ese uso está dentro de un `while` y el local se declaró fuera, el préstamo llega al final del `while`.

En el selfhost, una reasignación de un local existente registra una escritura del lugar en `bk_stmt_bind`, también si su tipo es `Copy`; una declaración `let` introduce otro binding. `bk_moved_loans` comprueba esa escritura frente a los préstamos vivos y produce `E_BORROW` si se solapan.

Conflictos, `E_BORROW`:

- `&mut` solapado con cualquier otro préstamo del mismo lugar (o de un prefijo o un campo).
- Mutar o mover un lugar mientras un préstamo suyo está vivo.
- Dos `&` compartidas no conflictúan.
- Devolver `&` de un local, o guardar una referencia más allá del ámbito de su referente.

El origen de una referencia es el lugar prestado. Un retorno `&'a T` solo es válido si `'a` es un lifetime de un parámetro (no de un local) y el origen lo satisface.

**Referencias.** `&mut T` se acepta donde se espera `&T`; lo contrario es `E_TYPE` (lo da `infer_program`). `&r` de una referencia represta. Mutar a través de `&T` es `E_BORROW`.

**`match`.** Si el escrutinio es un lugar con dueño y un brazo liga un payload no `Copy`, el lugar se mueve en ese brazo (`borrowck::match_moves`, que `ownership.rs` consulta mientras dure). Si el escrutinio está tras una referencia, los nombres del patrón quedan como préstamos (`Binding.from_ref`) y moverlos es `E_BORROW`. Los enums de otros módulos llegan por `set_extern_enums` (`modules::graph_enums`).

**Autoderef y autoref.** Leer un campo o llamar un método pela `&`/`&mut`. El receptor de un método se autoreferencia si la firma pide `&self` o `&mut self` y el valor es dueño. Un argumento de función no se autoreferencia: se escribe `&x`.

## Drop glue y elaboración

`elaborate_drops` (`src/drops.rs`, espejo `selfhost/ir/drops.sal`) no reanaliza escapes: lee los lugares movidos que publicó `check_borrows`.

- Al salir del ámbito, en cada `return` y al reasignar, suelta cada valor con dueño que sigue inicializado y no está movido en ese camino. Orden inverso de declaración. El valor de un `return` se calcula antes.
- Camino «quizá movido»: bandera `__live_nombre`, a 0 en el movimiento, y el drop detrás de `if __live == 1`.
- `Copy`, `&T` y `&mut T` no se sueltan.
- Un temporal con dueño se suelta al final de la sentencia si el destino no lo absorbió.
- `free(x)` escrito a mano consume `x`: no se suelta otra vez.

`lower_program` (`src/ir.rs`; C en `selfhost/emit/emit.sal`) emite una función `drop.<Tipo>` por tipo monomorfizado con dueño:

| Tipo | Glue |
|------|------|
| `String` | `sal_free` |
| `Bytes` | `sal_bytes_free` (una vista solo suelta la cabecera) |
| `Tensor` | `sal_tensor_free` / `sal_place_free` |
| `List[T]` / `Dict[K, V]` | recorre elementos (y claves `String`) con la glue del elemento y luego el bloque |
| struct | campos en orden de declaración; si tiene ≥2 campos, también el bloque (`sal_struct_free_box`) |
| enum | `switch` por tag |
| `&T`, `&mut T`, `Copy` | nada |

El bloque de una lista guarda el puntero a la glue del elemento (`sal_list_set_drop`), no un bit de dueño. Un `Dict` posee sus claves `String` y las suelta. `list_get` presta (`&T`); `list_get_mut` es `&mut T`; `pop` mueve; `push`/`set`/`put` toman `&mut` del contenedor, mueven el valor y devuelven `Unit`.

`x = x + …` con `x` un `String` con dueño (no una referencia) mueve `x` al `+` y lo reinicializa (SPEC «Expresiones»): `check_borrows` lo trata como una mutación de `x` (`E_BORROW` si hay un préstamo vivo), y ni la bajada ni `is_accumulate` (`src/drops.rs`) lo aplican cuando la hoja izquierda es una referencia. Así la reutilización del bloque en `sal_str_append`/`sal_str_append_n` no la ve nadie ([immutable-strings.md](immutable-strings.md)).

`x.clone()` copia: `strdup`, `sal_list_clone`, `sal_dict_clone`, `sal_bytes_clone`, `sal_enum_copy`, `sal_desc_copy` (struct describible) o `clone.<Struct>` (struct con glue y sin descriptor: `struct_clone_fn` en `src/ir.rs`, `struct_clone_fn` en `selfhost/ir/fuse.sal`). El receptor de `clone` solo se lee (`borrows_args`, `dr_borrows`). Efecto `alloc`. `Tensor.clone` es `E_TYPE`. Una lista de structs con `clone.<Struct>` la registra con `sal_list_set_clone`, y `sal_list_clone` la usa por elemento; una lista de listas clona cada lista interior.

Estado de la liberación mientras `insert_drops` siga en su puesto (lo que ya sigue la SPEC en los dos compiladores, con `corpus/drop_rebind.sal` en la paridad):

- Reasignar un local cuyas ligaduras son todas valores con dueño del mismo tipo (`String`, `List`, `Dict`, `Bytes`) suelta el valor viejo: `rebind` en `src/drops.rs`, `dr_rebind` en `selfhost/ir/drops.sal` (`__new<N>` = valor nuevo, suelta, asigna, repone `__live`).
- Un contenedor devuelto por una función de usuario lo suelta el llamante (`user_call_kind`; `retk:<fn>` en `drop_env_t`).
- `xs.clone()`, `d.clone()` y `b.clone()` son contenedores con dueño; `x.clone()` de un struct con `clone.<Struct>` lo suelta la cola de structs (`value_struct`; en el selfhost el tipo del local), y el de un struct describible, `struct_free` con su descriptor.
- `List.new[S]()` con `S` un struct con glue se suelta como cualquier lista (`list_glue_arg`; `glue:<S>` en `drop_env_t`). Una lista de listas, igual.
- El receptor de `push`/`set`/`put` no se mueve: un campo `List` al que se le hace `push` lo sigue soltando su struct.
- `xs.set(i, v)` suelta el elemento que sustituye, con la glue de la lista o con su descriptor, como `sal_list_free` (`sal_list_set`; volver a guardar el mismo valor no suelta nada): `tests/lang/collections/list_set_drops_old_element.sal`.
- `xs.pop()` de una lista de structs con glue da un struct con dueño que la cola suelta (`value_struct` en `src/ir.rs`); `drop.<S>` libera también su caja, así que `scan_struct_box_shell` no la vuelve a soltar (`list_pop_struct_is_dropped.sal`).
- `x.clone()` no mueve `x` (`collect_moved_expr` no marca el receptor de los clones), y el clon de un elemento (`xs.get(i).clone()`) lo suelta la cola con el tipo del receptor (`clone_keeps_source_owned.sal`, `clone_of_list_element_is_dropped.sal`). Para leer un struct escalarizado, `clone.<S>` y `sal_desc_copy` lo reciben en una caja temporal que se suelta tras la llamada con las cajas de sus structs anidados (`free_carrier` en `src/scalarize.rs`; `clone_nested_struct_frees_carrier.sal`).
- `read_only_params` comparte su punto fijo entre los módulos de un grafo, y ahí las funciones de los otros módulos llegan sin `infer_param_modes`: un parámetro por valor en modo `Inferred` cuenta como movido igual que `Take` (`takes_by_value`). Sin eso, `monomorphize_ext` soltaba otra vez el `TProgram` que había dado a `sp_run` (lo cubre `selfhost_instrumented_clean_on_imports`).
- Un `match` cuyos brazos dan el mismo struct con glue lo lleva en una caja hasta la unión; tras el `Switch` sus campos vuelven a ranuras y la caja se suelta (`merge_switch_arm` en `src/scalarize.rs`; un brazo que termina en `return` y el `default` vacío de un `match` exhaustivo no cuentan). Lo fija `match_arms_struct_value_is_released.sal`.
- El clon de un struct que vive en ranuras pasa a ranuras en cuanto sale de `clone.<S>`, y su caja se suelta: escribir después un campo (`f.body = nb`) ya no deja la caja atrás (`clone_then_field_write_releases_box.sal`).
- Una fn que devuelve un enum es fresca también si todo camino termina en un `x.clone()` (`enum_copy` en ese punto): `enum_expr_fresh` en `src/drops.rs` y `dr_enum_expr_fresh` en el selfhost. El llamante suelta el resultado.
- Un `match` con un brazo bloque (en la cola, como valor ligado o como sentencia) recorre cada brazo como una rama, igual que un `if`: los locales del bloque de un brazo se sueltan al final del brazo, y un local de fuera que mueve un brazo va detrás de su bandera en los demás (`walk_match_top`, `branch_matches` y `arm_block` en `src/drops.rs`; `dr_match_branches` y `dr_xform_branch_arms` en el selfhost). Un brazo sin bloque es un bloque con solo su cola. En el selfhost, el brazo que entrega un local lo marca también en `branch_moved`, como el arranque. Lo fijan `drop_match_arm_block_local.sal` y `drop_match_arm_branches.sal`.
- Las ligaduras se cuentan por camino: en las ramas de un `if` o de un `match` con brazo bloque, cada rama empieza desde la cuenta de antes y al final un nombre tiene la mayor de las ramas (`PathBinds` en `src/drops.rs`, `dr_path_next`/`dr_path_finish` en el selfhost). El mismo nombre ligado en dos ramas hermanas son dos locales, cada uno ligado una vez, como un `let` en cada brazo de un `if` de Rust. Antes contaba como religadura y un enum no se soltaba en ninguna (`drop_enum_same_name_sibling_branches.sal`; lo destapó un `LEAK` de stage1 instrumentado).
- Una cola de operador (`keep(s) + 1`) es una sentencia propia para lo que mueve, como una cola llamada: su bandera se limpia antes y el local se suelta detrás de ella en las otras ramas (`is_moving_tail`, `dr_is_moving_tail`; `drop_string_moved_by_operator_tail.sal`).
- Un local ligado a `xs.pop()` de un struct entrega su caja a la llamada que lo mueve (la suelta `drop.<S>` con los campos): ni `clear_moved` la suelta antes ni su liberación con bandera lleva la rama `struct_free_box` (`Ctx::popped` y `shell_box` en `src/drops.rs`, `popped` en `selfhost/ir/drops.sal`), y `scalarize` trata el resultado de `sal_list_pop` como una copia, cuya caja suelta `slots_of_moved`. Lo fija `list_pop_struct_moved_by_tail_call.sal`; antes, `n = use(q)` o la cola `use(q)` la soltaban dos veces en los dos compiladores.
- Un struct movido en una rama más honda que la que lo ligó (por la cola que lo devuelve, por una cola llamada u operador, o por una sentencia) se mueve solo en ese camino: va a `branch_moved` y se suelta con `drop.<S>` tras su bandera en los demás (`bind_branch` en `Facts::bind` y `arg` de `src/drops.rs`; `bbr` y `dr_soft` en el selfhost). `struct_glue_candidate` exige además que ningún movimiento escape (`escaped`): uno sin clave no limpia la bandera y la liberación se repetiría. Lo fija `drop_struct_moved_on_one_branch.sal`. Para espejarlo, el selfhost trata como movimiento el argumento por valor de un struct con glue (`dr_param_caller_keeps`, como `takes_by_value`) y calcula los parámetros de solo lectura también con las fns importadas del grafo (`dr_read_only_t` recibe `seen`, como `callees` en `read_only_params`): si no, `p.mkdir()` de un `StrPath` importado movía `p`.
- `sal emit ir` (`bootstrap_ir_text`) carga los modos de parámetro como `check_ownership` antes de publicar los movimientos de borrowck (`ownership::load_param_modes`): `operand_is_moved` los lee, y sin ellos `keep(s)` no movía `s` en `emit ir` pero sí al compilar, así que la IR mostrada (y la paridad) no era la compilada.
- El selfhost ya no usa la marca de «contenedor tainted» para decidir si suelta una lista: lo guardado se movió a ella.
- Un temporal prestado (`f(&g())` / `&mut g()`) se liga a un local (`Hoist` / `dr_hoist_unary`) y lo suelta la cola o la glue del struct. Dentro de un brazo de `match` se liga en el propio brazo y se suelta al acabarlo (en Rust el cuerpo de un brazo es un ámbito de temporales); los temporales del operando derecho de `&&` / `||` se ligan dentro del operando, en un bloque con su propio ámbito (`xform_logic`), porque solo corre si la izquierda lo deja (SPEC l. 258); los de una condición `elsif`, igual, dentro de la condición (`Hoist::expr` / `xform_expr`; `dr_hoist_arms` / `dr_xform_if_arms` en el selfhost). Un `Bytes` nuevo (`Bytes.new`, `bytes_view`, `Bytes.view_mem` o una fn que lo devuelve) leído por un builtin de `Bytes` (`len`, `ptr`, `get_*`, `to_str`, `bytes_view`) se liga a un temporal (`is_fresh_bytes`, `reads_container_arg`); `b.ptr()` y `bytes_view(b, ..)` solo leen `b` (`borrows_args`, `dr_borrows`). Un struct con glue que una fn o su constructor entrega, prestado a un `&self` / `&T` de otra fn (`Path.new(s).parent()`, `Response(raw).body()`), se liga a un temporal que suelta la cola de struct (`user_struct_temp`; `dr_user_struct_temp` con `dropfn:` / `dropkind:`). **Pendiente:** `owned_return_fns` / `owned_fns_fix` son el punto fijo mínimo, así que una fn recursiva que devuelve un `String` nuevo (`path_parent_i` de `std/io/fs.sal`) no cuenta como del llamante y su resultado se pierde. El máximo sería lo correcto, pero rompe el selfhost: el compilador acepta devolver un `&String` como `String` (`ar_expr` de `selfhost/sem/ownership.sal` acaba en `_ => flags`), y ese alias se usaba tras liberar el original. Antes hay que cerrar esa conversión implícita, que en Rust es un error de tipos.
- Sobreescribir un campo suelta el valor viejo; en un parámetro `&mut` o un alias de `list_get` se escribe in situ (`sal_struct_set`); tras llamar con un local scalarizado, `scalarize` repone los slots desde la caja. Si la llamada `&mut` está dentro de un cuerpo anidado (`if`, `while`, `match`), el local vive en una caja durante toda esa instrucción (`Pass::pin` / `unpin` en `src/scalarize.rs`): los slots se releen de la caja al empezar cada iteración, tras cada llamada y al salir, y un `return` suelta antes las cajas que siguen fijadas (`free_pinned`). Sin eso, lo escrito se perdía en la unión de ramas (`tests/lang/ownership/mut_ref_struct_in_loop.sal`, `mut_ref_struct_in_branch.sal`, `mut_ref_struct_return_in_loop.sal`).
- `&mut T` con `T` sin bloque propio (`Int`, `Float`, `Bool`, struct de un campo no genérico) pasa por una celda (SPEC «Liberación de locales»): `scalarize::FnSig::cell` / `tt_cell_flags` dicen qué parámetros la reciben. El llamante emite `sal_cell_new` antes de la llamada y `sal_cell_get` + `sal_cell_free` después, y vuelve a ligar `x`. El llamado lee `sal_cell_get` en el prólogo y emite `sal_cell_set` en cada `*p = v` y en cada `p.f = v` de un struct de un campo. Los `while` acarrean `x` (`collect_assigned_*` / `assigned_*`). Bajada: `src/ir.rs` (`CELL_PARAMS`, `CELL_SIGS`), `selfhost/ir/fuse.sal` (`Cx.cells`, `cell_args_new`) y C en `selfhost/emit/emit.sal` (`p__cell`, `cell_args_c`). Para una referencia guardada, ambos compiladores mantienen una celda para el local prestado y la liberan al salir; el selfhost detecta esos locales en `selfhost/ir/local_cells.sal`. Queda pendiente verificar todas las formas de lugar y ámbito.
- `&mut String` también pasa por una celda (`scalarize::is_cell_type`, `tt_is_cell`): `*s = v` suelta el texto viejo de la celda antes de escribir (`drop_cell_string` en `src/ir.rs`, `deref_string_drop` en `selfhost/ir/fuse.sal`). `xs.get_mut(i)` de una lista de un tipo en celda da la ranura del elemento (`sal_list_slot`; un `List[Bool]` da la dirección del byte con el bit bajo puesto, que `sal_cell_get`/`sal_cell_set` reconocen). `Mutex[String]` guarda el valor en una lista de un elemento y `enter` da su ranura (`sal_mutex_new_slot`). `check_borrows` solo rechaza `*r` de un no-`Copy` cuando se usa por valor; `*r = v` y `(*r).m()` no sacan nada (`bk_visit_unary` en el selfhost).

`drop.<Struct>` de un struct con dos o más campos suelta también el bloque (`sal_struct_free_box`); `drop.elem.<Struct>` es un envoltorio que lo llama (listas). Los campos enum, `List` y structs con glue de copia y liberación son propiedad de la caja según su tipo, tanto si llegan de un constructor como de una variable movida o una llamada; `sal_enum_free` los libera recursivamente y `sal_enum_copy` los clona. Al sustituir un campo, `sal_enum_set` libera el valor anterior. Las listas de enums poseen sus elementos también cuando se devuelven desde la función que las crea. Pendiente frente a la SPEC: la glue completa para los demás tipos de payload de enum. Un struct de varios campos sin glue (solo escalares) también es de la caja: `EnumMake` lo marca de tipo 3 y `sal_enum_set_struct_flat(v, i, n)` pone `sal_struct_free_box` como drop y `-n` (las palabras que copia `sal_enum_copy`) en el hueco del clone (`enum_struct_flat_fields` en src/ir.rs, `variant_field_struct_flat` en selfhost/ir/fuse.sal; `tests/lang/enums/enum_owns_flat_struct_payload.sal`). Siguen pendientes, con cualquier payload: la caja de un enum consumido por `match o` que mueve el payload fuera (parámetro o local), `x.clone()` de un enum y reasignar un local enum. Las reglas nuevas del borrow checker (`get` como `&T`, mover fuera de una referencia, payload de `match`, `&mut`→`&`) no están en `selfhost/sem/` y sus tests llevan `selfhost-xfail`.

Se retiran `fresh_kind`, `OWNED_FNS`, `read_only_params`, `last_consumed_params`, `caller_param_modes`, `field_reads` como análisis de «¿suelto esto?», la copia implícita `desc_copy` y `finish_module_box_shell_drops`. Los `tp_free_*` escritos a mano en `selfhost/` se eliminan: la glue los suelta y serían una doble liberación. Las asas `Int` (`str_as_int`, `vec_push`, `map_put`) siguen fuera del modelo.

## `@arena`

Sin cambio de runtime (`sal_arena_enter` / `sal_arena_leave`, páginas de 64 KiB, `free` interior no-op).

- Un parámetro que no sea de contenedor mutable es `&T` (o `Copy`).
- Lo que antes era `keep` es un parámetro `&mut List[…]` o `&mut Dict[…]`: guardar ahí copia al asignador del contenedor (`sal_arena_export`).
- El retorno no es de heap ni una referencia (el referente moriría con la región).
- La glue se ejecuta igual; dentro de la región sus `free` son no-op. `sal_arena_leave` va después.
- La comprobación vive en `check_borrows` (`src/borrowck.rs`), no en `ownership.rs`.

## Orden de pases

`parse` → `expand_interpolation` → `infer_program` (elisión, autoderef, `clone`) → `attrs` → `check_effects` → `check_borrows` → `check_devices` → `TypedProgram` → `elaborate_drops` → `lower_program` (glue) → `fuse_module` → LLVM → Clang.

## Tests

- `tests/frontend.rs`: árbol de `&T`, `&'a mut T`, `&x`, `&mut x`, `*x`, `&self`, y `fmt` idempotente.
- `tests/lang/ownership/`: conflicto `&`/`&mut` (`E_BORROW`), referencia que sobrevive al referente, devolver `&` de un local, elisión válida e inválida, préstamo que acaba antes de mutar, movimiento en bucle (`E_MOVED`), movimiento parcial, `clone` exigido.
- `tests/lang/drops/`: drop en rama, bucle, `match`, `return` y reasignación (`drop_reassign.sal`); contenedor que posee elementos de cualquier origen; contenedor devuelto por una función (`drop_fn_returns_container.sal`).
- `tests/lang/ownership/mut_ref_*.sal`: escritura a través de `&mut` de un valor sin bloque (celda), con `Int`, `Bool`, `Float`, struct de un campo, `String` envuelto, bucle en el llamado y `&mut` reenviada; `mut_ref_string_deref_assign.sal` (`*s = v` con `s: &mut String`), `list_get_mut_scalar_*.sal`, `list_get_mut_string.sal`, `deref_ref_read.sal` y `threads/mutex_string.sal`.
- `tests/lang/ownership/`: `clone` profundo (`clone_struct_list_field_deep.sal`, `clone_list_of_structs_deep.sal`), payload de `match` (`err_moved_match_payload.sal`, `match_ref_borrows_payload_ok.sal`).
- `tests/lang/attributes/`: `@arena` con `&T`, `keep` rechazado, retorno con `&` es `E_TYPE`.
- `tests/semantics.rs`, `tests/drops.rs`: el diagnóstico o la IR que la SPEC fija, escritos a mano.
- Selfhost: `cargo test --test selfhost_ir -- --test-threads=1`.

## Verificación

| Se tocó | Comando |
|---------|---------|
| Lexer / parser / fmt | `cargo test --test frontend --test parse_and_fmt` |
| Tipos y borrow checker | `cargo test --test semantics` |
| Drops y glue | `cargo test --test drops` |
| Conformidad | `cargo test --test lang` |
| Instrumentación | `cargo test --test instrument_sal --test instrument_leak` |
| Selfhost | `cargo test --test selfhost_ir -- --test-threads=1` |
