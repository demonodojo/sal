# Subplan: semántica

## Entrada

AST de [02-frontend-agents.md](02-frontend-agents.md).

## Salida

AST tipado, modo `borrow` o `take` de cada parámetro, efectos comprobados, diagnósticos con código y span.

## Archivos

`src/infer.rs`, `src/typed.rs`, `src/ownership.rs`, `src/effects.rs`, `src/device.rs`, `src/diag.rs`.

Tests: `tests/semantics.rs`.

## Tipos

Hindley–Milner con polimorfismo en el `let` y monomorfización en cada uso. Una `fn` genérica se comprueba una vez con los parámetros de tipo borrados a `Unknown` (`infer_fn` borra también los del cuerpo: `specialize::erase_body`); la llamada registra los tipos que fija (`TypeEnv.generic_calls`, `specialize::record_generic_call`) y su tipo es el retorno con ellos sustituidos. `src/specialize.rs` (`monomorphize`, tras `resolve_methods`; por módulo en `modules::monomorphize_graph`) crea `f__<tipos>` por cada combinación, renombra las llamadas, itera hasta el punto fijo y quita la original, así que no queda un tipo polimórfico en la IR. Todo o nada por genérica: una llamada sin tipos concretos la deja borrada (SPEC «Genéricos»). Entre módulos, cada uno especializa también las genéricas ajenas que llama (`MonoCtx`), y un barrido final quita las originales. Los `struct` genéricos se monomorfizan después (`monomorphize_structs`: `Box[String]` → `Box__String`). Cada copia recibe rangos `start`/`end` de span propios (`shift_spans`; línea y columna no cambian) porque las tablas span→tipo de la bajada son de todo el módulo. Detalle y estado: [14-protocolos-lambdas.md](14-protocolos-lambdas.md).

Un tensor es `Tensor[Elem, dims…] on lugar`. `Elem` es `F32`, `F16`, `BF16` o `I8`. `Float` es f64 y como elemento es `E_TENSOR_ELEM`. El lugar es `cpu`, `gpu`, `tpu` o una variable `p` de la firma. Esa variable se unifica entre los parámetros y el retorno de la misma función. Si en una llamada queda libre, el `to` del sitio de llamada la cierra. Si sigue libre, `E_TYPE`.

`matmul(A, B)`: `A` es `[M, K1]`, `B` es `[K2, N]`. Si `K1` y `K2` son estáticos y distintos, `E_SHAPE`. El resultado es `[M, N]` en el lugar de los dos operandos. Si los lugares no coinciden, `E_PLACE`: el compilador no inserta un `to`.

El resto, en el mismo lugar y el mismo elemento:

| Operación | Forma del resultado |
|-----------|---------------------|
| `map`, `relu`, `softmax` | la del argumento |
| `reduce` | se elimina el eje reducido |
| `transpose` | se intercambian los dos últimos ejes |
| `reshape` | las dimensiones escritas; si todas son estáticas, el producto tiene que coincidir (`E_SHAPE` si no) |
| `to q` | la misma forma, lugar `q` |

Operadores columnares (`+`, `-`, `*`, `/`, comparaciones, `-` unario, `!` sobre máscara `I8`): si un operando es `Tensor`, elemento a elemento. Escalar `Int`/`Float` se difunde; resultado aritmético mismo elemento y lugar; comparación → `Tensor[I8, …] on lugar`. Dos lugares sin `to`: `E_PLACE`. Elementos distintos: `E_TENSOR_ELEM`. Formas de rango 1 incompatibles: `E_SHAPE`.

Un `frame` con columnas `F32`/`F16`/`BF16`/`I8`/`String` y parámetro de lugar `p`: columna numérica → `Tensor[Elem, ?] on p`; `String` → `List[String]` en `cpu`. Con alguna columna `String`, el valor del frame es solo `cpu`; `to gpu`/`to tpu` sobre el frame: `E_PLACE`. `where(frame, mask)` exige máscara `Tensor[I8, ?] on p` y largo de fila unificado con las columnas.

Dos tensores del mismo elemento y la misma forma unifican eje a eje. `?` unifica con un estático y sigue siendo `?` en la firma; el estático no se filtra hacia el tipo público. Dentro de una función, un literal puede concretar el `?` para calcular tamaños, sin cambiar la firma.

`List[T]` es heap. El índice es `Int`. En el arranque, `T` es `Int`, `Float`, `Bool`, `String` o un struct describible (`S(…)` en el descriptor de lista). `list_get` / `list_pop` copian el elemento: `String` y struct describible entregan un bloque nuevo en el heap (`struct_copy` en instrumentación); el elemento dentro de la lista sigue siendo del dueño de la lista.

`Dict[K, V]` es heap. `K` es `Int` o `String`. `V` es cualquier tipo salvo `Tensor` y funciones (`E_TYPE` si `V` lo es). En runtime, `dict_new` codifica el kind del valor: escalares Copy, `String` (3), caja de enum sin parámetros de tipo (5) o bloque de un `struct` de dos o más campos describible (mismo criterio que el descriptor `S(…)` de listas). Primitivas: `dict_new`, `dict_put`, `dict_get` → `Option[V]` (exige `Option` en el entorno, normalmente vía preludio importado). `dict_get` copia en profundidad cuando hace falta (`String` con bit de dueño en el `Some`; enum y struct con las mismas reglas que `list_get` sobre listas dueñas).

Una constante de módulo (`Item::Const`, SPEC «Constantes») se evalúa en `src/consteval.rs` después de `qualify`, en el orden del grafo. El valor (`Int`, `Bool`, `Float` o `String`) y su tipo entran en `TypeEnv.consts` con el nombre ya cualificado; el import no copia el item. Un uso que no es un local ni un parámetro tiene ese tipo. Asignar al nombre es `E_TYPE`. Lo que no es expresión constante, un ciclo, `/` o `%` entre 0, o una anotación que no coincide, es `E_TYPE`.

Structs y enums se tipan por los `Item` del módulo actual y por los de cada módulo importado, en transitivo. Los nombres de tipo tienen que existir y la aridad de genéricos tiene que coincidir; no hay compatibilidad entre `Int` y `String` ni conversiones implícitas entre nominales distintos. `Nombre(args…)` construye un struct cuando `Nombre` es un struct; un identificador de variante construye un enum. `import` no copia cuerpos: el pase de inferencia construye un entorno con `fn`/`struct`/`enum` de todos los ficheros alcanzables. Ruta inexistente, choque de nombre o ciclo: `E_TYPE` en el span del `import`. El preludio no se inyecta; `Option` y `Result` salen de importar `std/prelude.sal`, no de un caso especial con otro nombre. `try` exige un `Result` y el error tiene que coincidir con el de la función que lo envuelve. Un `match` cubre las variantes del enum; si falta una y no hay `_`, `E_TYPE`. Sobre `Int`, los patrones son `_`, enteros y `IDENT` (ligado). Sobre `String`, `_`, literales `"…"`, `r"…"` (regex POSIX ERE; inválida `E_TYPE`; en `--target soso` `E_TARGET`) e `IDENT` (ligado). No mezclar familias de patrones ni con variantes de enum. Orden de fuente: **primer** brazo que coincide; sin coincidencia, `0` (`Int`). El tipo del `match` es el del primer brazo tipado; un brazo con bloque vale como `Expr::Block` (cola o `Unit`). La bajada a IR distingue cadena de `if` (literales y enlaces sobre `String`/`Int`) de `Switch` por tag (enums y colas solo variantes/`_`); ver [04-ir-runtime.md](04-ir-runtime.md).

Métodos (`method T.m(params)`, SPEC «Métodos»): son azúcar sobre `fn`, resuelta por `src/methods.rs` antes de ownership, efectos, drops e IR. `desugar_method_defs` renombra a `T__m` y antepone `self: T`; se ejecuta al cargar cada módulo, antes de `qualify`, y `compute_clashing_names` hace que `T__m` siga a `T` si el tipo se renombra a `modid__T`. `infer_call` acepta `Call{Field}`: infiere el receptor, busca `T__m` en `TypeEnv.fns` y deja la resolución en `TypeEnv.method_calls` (clave: span del `Call`); `resolve_methods` reescribe a `T__m(p, args)` (el `Ident` nuevo usa el span del `Call`, porque `Field` comparte span con su base). `static T.m` e `init T` se desazucaran igual (`FnDef.method_kind`, que sobrevive al desugar y llena `TypeEnv.statics` vía `register_fn`); `T.m(args)` se resuelve en `infer_static_call` cuando `T` es un tipo que ningún local sombrea, y `rename_expr_refs` renombra `T` si el tipo se renombra por choque. Los módulos se resuelven tras `qualify` en `resolve_graph_methods`; el fichero único, con `prepare_program`. Los tipos básicos usan la tabla `methods::builtin_method` (incluye `String.index_of` / `index_from` → `str_index_of` / `str_index_from`, `String.dup` → `strdup`, `String.to_int` → `str_as_int`, `Bytes.ptr` / `to_str` / `write_str`, estático `Bytes.view_mem`): `infer_method_call` liga el receptor una vez a un nombre sintético y delega en `infer_call` con el builtin, así que tipos, efectos, ownership y drops son los del builtin y el IR es idéntico al de la llamada plana. La resolución se ejecuta cuando algún módulo define un método o contiene una llamada `x.m(…)` (`has_field_calls`); si no, no se ejecuta nada. El selfhost todavía no los admite (`# selfhost-xfail:` en `tests/lang/methods/`).

La lambda de una sentencia se tipa al usarla. `x => e` no es un valor de primera clase que se guarde: si se liga a un `let` o se devuelve, `E_TYPE` en este arranque. El parámetro no lleva tipo escrito; sale del callback (`map`).

Protocolos (`src/protocols.rs`, SPEC «Protocolos»): el contrato vive en `protocol P`; la reclamación en `impl P, …` en la cabecera de un `struct`/`enum` sin genéricos. Al cerrar el módulo se cotejan los `method T.m` ya definidos (misma firma que el protocolo). Solo un tipo que reclamó `P` coacciona a `any[P]` (mueve a caja `{vtable, payload}`, exige `alloc`). `x.m()` sobre `any[P]` no pasa por `resolve_methods`: la vtable apunta a los `T__m` existentes. Cotas `T: P` quedan fuera de este corte.

`&&` y `||` toman operandos de verdad (`Bool`, o `Int` donde distinto de cero es verdadero, como en una condición) y dan `Bool`. Un `Float`, `String` u otro tipo es `E_TYPE`. Una comparación escalar sigue siendo `Int`.

Un `if` sin `else` tiene tipo `Unit`, aunque tenga `elsif`; la rama then y cada `elsif` se comprueban igual. Un `if` con `else` tiene el tipo de la rama then. Las ramas `elsif` se comprueban como el `else`.

`return expr` termina la función con el tipo declarado; `return` sin expresión solo con retorno `Unit`. Prohibido dentro de `on` y de lambda (`E_TYPE`). Código inalcanzable tras un retorno seguro en el mismo bloque: `E_TYPE`. Bloque de función sin cola y sin retorno en todos los caminos: si el retorno no es `Unit`, `E_TYPE`. Rama de `if`/`match` con `return` diverge y no cuenta en el tipo de la expresión.

Cada expresión del AST tipado guarda su tipo, su lugar y el span. `sal emit typed` imprime eso. `sal fmt` escribe en las firmas públicas el `borrow` o `take` que haya inferido el pase de ownership, para que no cambie en silencio. Si ese `borrow` era inferido y `last_consumed_params` marca el parámetro como consumido, no lo escribe: `caller_param_modes` solo lo promueve a `take` para los llamadores mientras el modo sigue ausente, y escribirlo cambiaría la IR.

## Ownership

Pase léxico, sentencia a sentencia, después de tipar. No hay lifetimes en el fuente.

- `Int`, `Float`, `Bool` y un struct cuyos campos lo son se copian.
- Lo demás es único y se mueve.
- Un préstamo dura el argumento de la llamada. No hay un local de tipo referencia.
- Mutar un campo (`user.name = …`) exige ser el dueño único.
- `to` de un valor único consume el origen. Volver a usarlo es `E_MOVED`.
- El modo del parámetro sale del cuerpo. Si solo se lee para pasarlo a una llamada, `borrow`. Si el cuerpo lo mueve, lo guarda o lo devuelve, `take`. El AST tipado publica ese modo. El selfhost lo infiere con la misma regla (`selfhost/sem/ownership.sal`), y la bajada a IR lo infiere también en el bootstrap antes de decidir qué argumentos quedan movidos. El pase `check_ownership` recibe la salida de `infer_program`: el tipo de cada expresión sale de ahí, los argumentos en posición `take` de funciones de usuario, el valor de `list_push`/`list_set`/`dict_put` y los argumentos de constructores de `struct` o variante se mueven, y los modos se infieren por punto fijo entre funciones (`selfhost/sem/ownership.sal` lo espeja con `own_mode_table`). El movimiento parcial de campos (`n = p.name`, `return p.campo`) marca ese campo como movido (`p.campo` en el conjunto de movidos; `p = …` o `p.campo = …` lo reponen). Las banderas de drop (`__live_x`) las inserta `src/drops.rs` (espejo `selfhost/ir/drops.sal`) para locales que escapan solo en sentencias completas de su mismo nivel de bucle: la sentencia que mueve baja la bandera y el drop de fin de ámbito o de `return` queda detrás de `if __live_x == 1`. Las colas de bloque, los brazos de `match`, los bloques `on` y los movimientos en un bucle más interno siguen sin soltarse. Los temporales (un `String` fresco pasado a un builtin de lectura) se ligan antes de la sentencia (`Hoist` en `src/drops.rs`, `dr_hoist_*` en el espejo). Los argumentos movidos a un constructor de `struct` no se liberan otra vez como local en ese ámbito. El pase de drops añade un análisis aparte de «parámetro de solo lectura» (punto fijo optimista, `src/drops.rs`) que decide si un valor pasado a una función de usuario sigue siendo del llamante.

### Liberación (resumen; autoridad en SPEC «Liberación de locales»)

Tras ownership, `insert_drops` (`src/drops.rs`, espejo `selfhost/ir/drops.sal`) reescribe el AST con `free` / `list_free` / `dict_free` / `enum_free` / `bytes_free` / `tensor_free` / `struct_free` / `struct_free_box` antes de bajar. Un local se suelta solo si se liga una vez a un valor fresco y no escapa (ver SPEC). Contenedores de `String` (lista, dict o campo `List[String]` / `Dict[…, String]` en un struct) solo entran si todo lo guardado es fresco; si no, se deja sin liberar. `dict_get` entrega un `Option` fresco: `enum_free` de la caja si no escapa. `bytes_view` / `bytes_view_mem` liberan solo la cabecera de la vista. Funciones de usuario en `OWNED_FNS` devuelven `String` siempre fresco (cada `return` y la cola; `if`/`match` de nivel superior vale si **cada** rama es fresca; un `while` de nivel superior las excluye).

**Copia de struct desde lista.** Un local ligado a `list_get`/`list_pop` de `List[P]` describible es kind `struct:…` con bandera `__live` si puede escapar en sentencias del mismo nivel de bucle. Al pasarlo como argumento hay que distinguir destino:

| Destino | Caja del bloque copiado |
|---------|-------------------------|
| `list_push`, `list_set`, `dict_put` o constructor de struct (`hard_move`) | La absorbe el contenedor o el ctor; bandera a 0; no `struct_free` ni `struct_free_box`. |
| Función de usuario cuyo parámetro struct se escalariza (campos en registros) | `ArgMove::TakeFields`, conjunto `box_shell`; los campos se mueven; la caja queda huérfana. |
| Solo lectura (builtin lector, arg read-only, `borrow`) | No mueve; `ArgMove::Read`. |

En el AST, `mark_box_shell_calls` recorre sentencias **y la cola** del bloque; `clear_moved` puede emitir `struct_free_box` al mover; con bandera, `release` hace `if __live == 1` → `struct_free(desc)` else → `struct_free_box` cuando `box_shell`. Runtime: `sal_struct_free_box` libera solo el `malloc` del bloque; `sal_struct_free` recorre campos y la caja.

Tras `lower_program`, `fuse_module` (`src/fuse.rs`) puede añadir `sal_struct_free_box` en IR (`finish_module_box_shell_drops` en `src/ir.rs`): registra destinos de `sal_list_get`/`sal_list_pop` solo en `loop_depth == 0`, marca el shell al pasar la copia a una llamada no-`sal_` que no transfiere caja, e inserta la llamada **inmediatamente antes** del último `Return` (nunca después). Copias definidas dentro de un `While`/`If` anidado no entran en ese shell de fin de función. Al compilar módulos bajo `selfhost/`, `compile.rs` usa `fuse_module_opts(..., box_shell_drops: false)` para no aplicar ese pase al código del autohospedador (SSA en bucles del compilador); `sal emit ir` y tests del arranque siguen con el pase activo.

**Cola de struct.** La emite `lower_fn` (`src/ir.rs`, `selfhost/ir/fuse.sal`, C en `emit.sal`) después del cuerpo y antes del `return`. Suelta `String`, enum, `Bytes`, listas/dicts escalares, **`List[P]`** con `P` struct describible (misma regla `container_tainted` que `List[String]`), contenedores de `String` no manchados y structs anidados; omite rutas en `strings_moved` (campo `String` o enum entregado) y contenedores en `heap_moved`. Los movimientos por llamada se leen con la vista de los llamadores: `caller_param_modes` promueve a `take` el parámetro de modo inferido que el llamado consume (`last_consumed_params`: lo devuelve, lo guarda o se lo pasa a quien lo consume, también si ese consumidor está en otro módulo del grafo). Un campo Copy (`return p.n`) escapa para drops, pero no consume, y la cola del llamador sigue soltando los campos propios. `field_reads` usa esa misma vista (`caller_modes`), así la cola del llamado no suelta un campo que el cuerpo ya entregó a un parámetro que escapa. Un parámetro cuyo cuerpo lo pasa suelto a un builtin que libera (`list_free(p)`, `enum_free(p)`) también es `take` para los llamadores (`releases_param_fields`). Un local ligado al resultado de una llamada que devuelve uno de sus parámetros (cola de alguna rama) no se libera cuando ese argumento es un parámetro prestado del llamador o un campo: el resultado es un alias (`list_alias_locals` / `call_aliases_borrowed_arg`; en el selfhost, `method FRC.alias`, que consulta las funciones de todo el grafo porque el llamado puede estar en otro módulo). No se inserta una copia de la carga al llamar: la bajada pasa el struct entero y `sal_enum_copy` comparte los campos que el descriptor marca como prestados, así que copiar la caja no separa el bloque. El alias es lo que evita soltarlo dos veces. Entrada: `drops::last_read_only_params`; en el selfhost, `caller:` en `modes_fill_t` con `mark_escaping_params`. La cola propia del llamado sigue con `param_modes`. Huecos que siguen sin liberarse: campos de caja enum no frescos, `list_pop` de lista no dueña, `String` como asa `Int`, alias `z = k` sobre la misma copia. Tests: `tests/drops.rs`, `tests/glue_escape/`, `tests/lang/drops/` (p. ej. `drop_list_struct_box_pass_fn.sal`, `drop_struct_list_field.sal`, `drop_struct_freed_by_callee_of_callee.sal`), `tests/instrument_sal.rs` (`struct_copy` en estricto).

`@shared` no se infiere. Si el programa necesitaría guardar un préstamo, el error pide devolver por movimiento. No se inventa un lifetime.

## Efectos y lugares

El cuerpo no puede hacer un efecto que la firma no declara (`E_EFFECT`).

| Construcción | Efectos |
|--------------|---------|
| `load` | `io`, `alloc` |
| literal `tensor`, `String`, `List`, `+` con izquierdo `String` | `alloc` |

`+` con el izquierdo `String` devuelve `String` y exige `String` a la derecha (`E_TYPE` si no lo es, y también si el derecho es `String` y el izquierdo no). Presta los dos operandos: ninguno cambia. El acumulador `x = x + …` mueve `x` (SPEC «Expresiones»); la bajada está en [04-ir-runtime.md](04-ir-runtime.md) y en [immutable-strings.md](immutable-strings.md).
| `on gpu`, `to gpu` | `gpu` |
| `on tpu`, `to tpu` | `tpu` |
| `panic` | `panic` |

`on tpu kernel` es `E_DEVICE`. Un tensor cuyo lugar no es el del `on` que lo usa, y que no es un escalar `copy` pasado por valor, es `E_PLACE`.

## `@arena` y `keep`

`src/attrs.rs` (`check_arena_fns`) rechaza el retorno de heap y `@arena` junto con `@no_heap` (`E_TYPE`). `src/ownership.rs` infiere el modo y, en una función `@arena`, un parámetro que no es `keep` no puede mutarse ni cederse: el punto fijo de «esta posición muta» es el de los parámetros de solo lectura (empieza en falso y crece; un builtin de contenedor o una posición `take`, `keep` o ya mutadora marca el argumento). El mensaje es `@arena parameter \`x\` is read-only`. Un parámetro sin usar pasa a `borrow`, no a `take`, para no liberar el valor del llamador.

`keep` solo en `@arena` y solo sobre `List` o `Dict` (`keep parameter requires @arena`, `keep parameter \`x\` must be a List or Dict`). Pasarlo a una posición `take` o mutadora que no sea `keep` es `E_TYPE`. Guardar un `Tensor` o un struct sin copia es `E_TYPE`. El selfhost hace el mismo corte en `selfhost/sem/attrs.sal` y `selfhost/sem/ownership.sal`.

## Hecho cuando

`tests/semantics.rs` falla con el código exacto:

- `Tensor[Float, 2, 2] on cpu` → `E_TENSOR_ELEM`
- `matmul` con K estáticos distintos → `E_SHAPE`
- mezcla de lugares sin `to` → `E_PLACE`
- segundo uso después de `to gpu x` → `E_MOVED`
- `on tpu kernel` → `E_DEVICE`
- `load` sin `! io, alloc` → `E_EFFECT`
- la firma tipada de un `forward` que solo lee los pesos publica `borrow` en esos parámetros
