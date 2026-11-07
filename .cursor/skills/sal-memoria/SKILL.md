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

La autoridad es [SPEC.md](../../../SPEC.md) («Liberación de locales»). Resumen operativo en [docs/subplans/03-semantics.md](../../../docs/subplans/03-semantics.md) y [04-ir-runtime.md](../../../docs/subplans/04-ir-runtime.md). Si la SPEC y el código divergen, no cierres una a ojo: avisa y ciérralas en el mismo cambio.

Invariante: una fuga es aceptable; una doble liberación o un uso tras liberar no. Ante la duda, no sueltes.

## Dos sitios, los dos compiladores

| Qué | Arranque | Selfhost |
|-----|----------|----------|
| Locales (`free`, `list_free`, `dict_free`, `enum_free`, `bytes_free`, `tensor_free`, `struct_free`, `struct_free_box`), antes de bajar | `src/drops.rs` (`insert_drops`) | `selfhost/ir/drops.sal` (`drops_body_t`) |
| Cola de campos de un struct (IR) | `src/ir.rs` (`free_struct_strings`, `field_reads`, `emit_struct_string_drops`, `field_drop`) | `selfhost/ir/frc.sal` (`FRC` y sus métodos, `drop_fields`, `fr_*`), `selfhost/ir/desc.sal` (`drop_field_fn`), `selfhost/ir/fuse.sal` (`emit_struct_drops`) |
| Caja de copia `list_get`/`list_pop` tras llamada escalarizada (IR, tras fusión) | `src/ir.rs` (`finish_module_box_shell_drops`), invocado desde `src/fuse.rs` (`fuse_module` / `fuse_module_opts`) | el arranque en `emit ir` / tests; ver nota selfhost abajo |
| Cola en C (stage2) | — | `selfhost/emit/emit.sal` (`emit_c_struct_scope_frees`, importa `frc` y `desc`) |
| Movimiento parcial y `borrow`/`take` | `src/ownership.rs` | `selfhost/sem/ownership.sal` |
| Runtime (`sal_dict_*`, `sal_enum_*`, vistas `bytes`, `sal_tensor_free`) | `runtime/sal_runtime.c` | el mismo |

`insert_drops` va entre `TypedProgram` y `lower_program`. No compruebes en un pase posterior lo que este ya decide. El selfhost arma `drop_env_t` en `fuse.sal` (`enum:`, `struct:`, `desc:`, `ownfn:`) para `dr_*`.

Un cambio de esta zona toca los dos compiladores en el mismo cambio (y runtime si cambian kinds o dueño). La IR tiene que coincidir; el emisor C del selfhost debe espejar la cola cuando stage2 ejecuta.

## Qué suelta `insert_drops`

Un local se libera si se liga una sola vez a un valor **fresco** (`fresh_kind`) y no escapa:

- **`String`**: literal, builtins de cadena fresca, o `+` con operando `String`.
- **`List` / `Dict`**: tipos y dueño según SPEC; contenedor tainted si guarda un nombre o resultado de función de usuario (`Facts.tainted` / `container_tainted` en cola de struct).
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
- **`list_push` / `list_set` / `dict_put`**: prestados como sentencia o `_nombre`; `ys = xs.push(v)` escapa.

### Copia de struct (`list_get` / `list_pop`)

Un `list_get` de lista de struct describible entrega una **caja** (`struct_copy` en instrumentación) más campos copiados. Hay que distinguir el destino del argumento:

| Destino | Qué pasa con la caja | Local / bandera |
|---------|----------------------|-----------------|
| `list_push`, `list_set`, `dict_put` o ctor de struct (`hard_move`) | La caja la absorbe el contenedor o el ctor | `__live` a 0; no `struct_free` ni `struct_free_box` |
| Función de usuario (parámetro struct; la bajada **escalariza** campos) | Se mueven los campos; la caja queda huérfana | `ArgMove::TakeFields`, `box_shell`; suelta con `struct_free_box` (solo el `malloc` del bloque) |
| Solo lectura (`borrow`, builtin lector, arg read-only) | No mueve | `ArgMove::Read` |

En **`src/drops.rs`**: `mark_box_shell_calls` recorre sentencias **y el tail** del bloque; `clear_moved` puede emitir `struct_free_box` al mover; `release` con bandera usa `if __live == 1` → `struct_free` else → `struct_free_box` cuando `box_shell`.

En **`src/ir.rs`**, tras **`fuse_module`**: `scan_struct_box_shell` registra destinos de `sal_list_get`/`sal_list_pop` (solo si `loop_depth == 0` en esa función), marca el shell al pasar la copia a una llamada no-`sal_` que no transfiere caja, e inserta `sal_struct_free_box` **inmediatamente antes** del último `Return` (nunca después). Copias ligadas dentro de un `While`/`If` anidado no entran en el shell de fin de función (evita LLVM inválido).

Runtime: **`sal_struct_free_box`** libera solo el bloque; **`sal_struct_free(desc)`** recorre campos y la caja (copia completa con bandera a 1).

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
- Descriptores: `List[TStmt]` es `S(isEsiEiii)` (`+` si es dueña). `list_get` devuelve una copia y sus cajas `E` se copian con `sal_enum_copy`, que solo copia los campos poseídos. `List[TFn]`, `List[TProgram]`, `List[TStruct]`, `List[TEnum]`, `List[TArm]` y `List[TElsif]` son `i`: `list_get` devuelve el mismo valor, y `list_set`/`list_free` no sueltan el elemento.
- **`sal_bytes_view`**: flag `view`; `sal_bytes_free` no libera el `data` ajeno.
- **`sal_dict_free` / `dict_put`**: `dict_val_release` para `String`, enum (kind 5) y struct empaquetado (kind `4|…`).

## Cola de struct: vista de los llamadores

- Un argumento struct pasado a un parámetro de modo inferido que el llamado consume cuenta como movido para la cola del llamador, aunque el modo inferido sea `borrow`. Consumir es devolverlo, guardarlo o pasárselo a quien lo consume. Un campo Copy (`return p.n`) escapa para drops (el llamado no lo suelta) pero no consume: si contara, la cola dejaría de soltar el `String` que nadie más suelta. `field_reads` usa esa misma vista. Un parámetro que el cuerpo pasa suelto a un builtin que libera (`list_free(p)`) también es `take` para los llamadores.
- Arranque: `caller_param_modes` en `src/ir.rs`, a partir de `drops::last_consumed_params`. El cálculo incluye las funciones de los otros módulos del grafo, así un consumidor importado (`tp_free_shallow`) también cuenta. Selfhost: `DC.took` en `drops.sal` (un campo Copy escapa y no entra; el flag vive en la lista `copy_ok`, alineada con `Type.field`) y `mark_escaping_params` en `modes_fill_t` (`fuse.sal`), que promueve solo lo consumido. Los flags de cada función van en un vector por índice (`dr_cons_list`), no en un mapa por nombre. Un cuerpo cuyo valor es el parámetro (`pin_tail_take`, y `own_pin_tail` en ownership) es `take` para los llamadores. Tests: `struct_glue_*` en `tests/drops.rs`, `tests/glue_escape/` en `PARITY_FILES`, `corpus/param_modes.sal` y `tests/lang/drops/drop_struct_freed_by_callee_of_callee.sal`.

## Siguen sin liberarse (SPEC)

Campos de caja enum **no frescos**; **`list_pop`** de lista no dueña; **`String` como asa `Int`**; **`z = k`** sobre la misma copia. Tensores en región fusionada: `IrInst::Drop` + `sal_place_free` en bajada LLVM, no el mismo camino que un local escalar.

## Verificar

| Se tocó | Comando |
|---------|---------|
| `src/drops.rs`, `src/ir.rs`, `src/fuse.rs`, `tests/drops.rs` | `cargo test --test drops` |
| `tests/lang/drops/` (p. ej. `drop_list_struct_box_pass_fn.sal`, `drop_struct_list_field.sal`) | `cargo test --test lang` (`# exit:` = retorno de `main`, no “OK = 0”; ver [sal-conformidad](../sal-conformidad/SKILL.md)) |
| Selfhost drops/fuse/emit | `cargo run -- parity <fichero>`; `cargo test --test selfhost_ir -- --test-threads=1` (paridad IR puede diferir en programas con caja si solo el arranque aplica el pase IR) |
| Runtime | `tests/instrument_sal.rs` (`struct_copy`, caja tras `list_get`) + `tests/drops.rs` |

`SAL_INSTRUMENT_STRICT=1` tras `sal build --instrument`: stderr sin `LEAK` ni `struct_copy` residual en programas que el compilador debe cerrar (`USE_AFTER_FREE`, `DOUBLE_FREE` siguen siendo fallo duro).

Un test que sigue pasando con el código viejo no cuenta. Quita `# xfail:` / `# selfhost-xfail:` al cerrar un hueco en el mismo cambio.
