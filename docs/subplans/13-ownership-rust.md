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

Conflictos, `E_BORROW`:

- `&mut` solapado con cualquier otro préstamo del mismo lugar (o de un prefijo o un campo).
- Mutar o mover un lugar mientras un préstamo suyo está vivo.
- Dos `&` compartidas no conflictúan.
- Devolver `&` de un local, o guardar una referencia más allá del ámbito de su referente.

El origen de una referencia es el lugar prestado. Un retorno `&'a T` solo es válido si `'a` es un lifetime de un parámetro (no de un local) y el origen lo satisface.

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
| struct | campos en orden de declaración |
| enum | `switch` por tag |
| `&T`, `&mut T`, `Copy` | nada |

El bloque de una lista guarda el puntero a la glue del elemento (`sal_list_set_drop`), no un bit de dueño. Un `Dict` posee sus claves `String` y las suelta. `list_get` presta (`&T`); `list_get_mut` es `&mut T`; `pop` mueve; `push`/`set`/`put` toman `&mut` del contenedor, mueven el valor y devuelven `Unit`.

`x.clone()` copia: `strdup`, `sal_list_clone`, `sal_dict_clone`, `sal_bytes_clone`, `sal_enum_copy` o `sal_desc_copy`. Efecto `alloc`. `Tensor.clone` es `E_TYPE`.

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
- `tests/lang/drops/`: drop en rama, bucle, `match`, `return` y reasignación; contenedor que posee elementos de cualquier origen.
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
