---
name: sal-memoria
description: >-
  Guía la liberación de locales en sal: el pase insert_drops, la cola de
  campos de un struct, el espejo en el selfhost y la invariante de no liberar
  dos veces ni usar tras liberar. Usar al editar src/drops.rs, la cola de
  structs en src/ir.rs, runtime/sal_runtime.c, selfhost/drops.sal, la cola en
  selfhost/fuse.sal, emit.sal (cola C), tests/drops.rs, tests/lang/drops, o al
  hablar de free, list_free, dict_free, enum_free, bytes_free, tensor_free,
  fugas, LEAK, uso tras liberar o doble liberación.
---

# Memoria

La autoridad es [SPEC.md](../../../SPEC.md) («Liberación de locales»). Resumen operativo en [docs/subplans/03-semantics.md](../../../docs/subplans/03-semantics.md) y [04-ir-runtime.md](../../../docs/subplans/04-ir-runtime.md). Si la SPEC y el código divergen, no cierres una a ojo: avisa y ciérralas en el mismo cambio.

Invariante: una fuga es aceptable; una doble liberación o un uso tras liberar no. Ante la duda, no sueltes.

## Dos sitios, los dos compiladores

| Qué | Arranque | Selfhost |
|-----|----------|----------|
| Locales (`free`, `list_free`, `dict_free`, `enum_free`, `bytes_free`, `tensor_free`, `struct_free`), antes de bajar | `src/drops.rs` (`insert_drops`) | `selfhost/drops.sal` (`drops_body_t`) |
| Cola de campos de un struct (IR) | `src/ir.rs` (`free_struct_strings`, `field_reads`, `emit_struct_string_drops`) | `selfhost/fuse.sal` (`drop_fields`, `fr_*`, `emit_struct_drops`) |
| Cola en C (stage2) | — | `selfhost/emit.sal` (`emit_c_struct_scope_frees`, importa helpers de `fuse`) |
| Movimiento parcial y `borrow`/`take` | `src/ownership.rs` | `selfhost/ownership.sal` |
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

## Cola de un struct

La emite `lower_fn` (y el C de `emit.sal`) después del cuerpo y antes del `return`. Un struct local o parámetro `take` suelta:

- `String` (`sal_free`), enum (`sal_enum_free`), `Bytes`, `List`/`Dict` de escalares Copy.
- **`List[String]`** y **`Dict[..., String]`** si **`container_tainted`** no marca el campo (solo valores frescos guardados).
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
- **`sal_bytes_view`**: flag `view`; `sal_bytes_free` no libera el `data` ajeno.
- **`sal_dict_free` / `dict_put`**: `dict_val_release` para `String`, enum (kind 5) y struct empaquetado (kind `4|…`).

## Siguen sin liberarse (SPEC)

Campos de caja enum **no frescos**; **`list_pop`** de lista no dueña; **`String` como asa `Int`**; **`z = k`** sobre la misma copia. Tensores en región fusionada: `IrInst::Drop` + `sal_place_free` en bajada LLVM, no el mismo camino que un local escalar.

## Verificar

| Se tocó | Comando |
|---------|---------|
| `src/drops.rs`, `src/ir.rs`, `tests/drops.rs` | `cargo test --test drops` |
| `tests/lang/drops/` | `cargo test --test lang` |
| Selfhost drops/fuse/emit | `cargo run -- emit ir` vs `parity`; `cargo test --test selfhost_ir -- --test-threads=1` |
| Runtime | tests de drops + instrumentación |

`SAL_INSTRUMENT_STRICT=1` tras `sal build --instrument`: stderr (`LEAK`, `USE_AFTER_FREE`, `DOUBLE_FREE`).

Un test que sigue pasando con el código viejo no cuenta. Quita `# xfail:` / `# selfhost-xfail:` al cerrar un hueco en el mismo cambio.
