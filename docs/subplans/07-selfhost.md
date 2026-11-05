# Subplan: autohospedaje

## Entrada

El compilador de arranque en Rust, ya capaz de llevar un programa sal a un binario nativo según [04-ir-runtime.md](04-ir-runtime.md) y [06-toolchain.md](06-toolchain.md).

## Salida

`selfhost/` escrito en sal, `std/prelude.sal`, `corpus/` y `tests/selfhost_ir.rs`.

## Qué se hospeda

`selfhost/` es el mismo compilador, no un dialecto reducido ni un programa que devuelve 0. Lee fuente, construye el AST de la gramática recursiva por la izquierda, tipa, posee, baja a la misma IR y escribe el objeto. Usa `Result`, efectos `io` y `alloc`, y la caché por módulo. El parser de sal reconoce las mismas producciones que `src/parser.rs`: listas y operadores crecen por la izquierda; `1 - 2 - 3` tiene el mismo árbol en las dos implementaciones. `d"…"` (`DSTRING`) se parsea como la misma llamada `strdup("…")` que en el arranque.

Los pases viven en módulos de `selfhost/` con los mismos cortes que el arranque: lexer, parser, tipos, ownership, efectos, lugares, IR, fusión, emisión. Las primitivas de tensor no se reimplementan en sal: se llaman, y el runtime del arranque las resuelve, igual que en cualquier otro programa.

`std/prelude.sal` define `Option`, `Result` y las firmas reales de `List`, `map`, `reduce`, `matmul`, `softmax`, `reshape`, `transpose`, `relu`, `load` y `print`. `load` se escribe `load[F32, 4, 4](path)`, que es la producción `postfix` de la SPEC. Los cuerpos de las primitivas de tensor no son un `t` devuelto tal cual: o delegan en la primitiva del runtime o no se declaran como funciones sal ordinarias que el usuario podría sombrear. El compilador las reconoce por nombre después de resolver el preludio.

## Corpus

`corpus/` tiene al menos:

- movimientos (`E_MOVED` no dispara; el programa es legal y usa un valor una sola vez)
- efectos declarados
- lugares, con `to` explícito
- un `forward` fusionado del que se mira `peak_bytes`
- un `load` de un `.salt`
- un programa pensado para compilarse con `--instrument`

## Tres cadenas

```text
bootstrap Rust  → compila selfhost/     → binario stage1
stage1          → compila selfhost/     → binario stage2
bootstrap, stage1 y stage2 compilan cada fichero de corpus/
```

La IR normalizada de cada programa del corpus es idéntica en las tres cadenas. Normalizar quita nombres frescos que no sean estructurales y deja operaciones, lugares, `fused` y `peak_bytes`.

`selfhost/` compilado con `--instrument` termina sin `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN` ni `INF`. Compilar no exige GPU ni TPU. Las pruebas del paquete, compiladas por ese binario, sí ejecutan `on gpu` y `on tpu` en el heap del runtime.

## Hecho cuando

`tests/selfhost_ir.rs` compara las tres IR del corpus y falla si una difiere. El binario de stage1 no es un stub que reimprime la IR del arranque: se obtiene compilando `selfhost/` con el arranque, y stage2 compilando `selfhost/` con stage1.

## Estado

El selfhost **no está completo**. Lo que hay hoy:

| Pase | Selfhost | Notas |
| --- | --- | --- |
| Lexer | `lexer.sal` (en sal, ya no `lex_src` de C) | Tokens de la SPEC incl. `let`, `match`, `try`, `=>`, bit ops. |
| Parser | `parser.sal` | Precedencias de la SPEC, `let`, lambda, `try`, `match`, `enum`, `frame`, parámetros de tipo, asignación a campos. Los tipos siguen siendo texto. |
| Efectos | `effects.sal` | Paridad con `src/effects.rs`. |
| Tipos | `infer.sal` | Solo escalares (`Int`/`Float`/`Bool`/`String`/`Unit`): variables, retornos, ramas de `if`, aridad y argumentos de funciones de usuario, operadores. `matmul` entre tensores con tipo declarado comprueba elemento, rango y dimensión interior (`E_SHAPE`); `transpose` (rango), `reshape` (volumen con literales) y `softmax` (eje literal) también, y `relu`/`silu`/`rope` conservan el tipo. Resto de tensores (`attention`, `qmatmul`, `reduce`, `rms_norm`), genéricos y builtins sin firma modelada son `Unknown`. |
| Ownership | `ownership.sal` | `E_MOVED`; tipos de valor simplificados. |
| Dispositivos | `device.sal` | `E_PLACE`/`E_DEVICE` con la misma lógica que `src/device.rs`; el parser reconoce `on <place> kernel`. |
| IR | `fuse.sal`, `tensorops.sal` | `if`/`while` con carries, struct/aggregate, drops de structs con `String`, drops de locales (`drops.sal`: `String`, `List`, `Dict`, `Bytes`, caja de enum), forma de `matmul`, `reshape`, `transpose`, `attention`, `rope`, `silu`, `tensor_append`, contenedores. |
| Backend | `emit.sal` emite IR de texto y **C** | No hay LLVM ni objeto propio. Dos caminos a C: desde el AST (escalares, strings, structs, bytes, `syscall`) y, para funciones con tensores, desde la IR (`cir_*`): `load`, `matmul`, `rope`, `silu`, `softmax`, `attention`, ops elemento a elemento, `to <place>`, literales. Los buffers de tensor llevan su longitud en una cabecera de 16 bytes y no se liberan. |

Los ejecutables del selfhost coinciden con los del arranque (código de salida y stdout) en los ejemplos y corpus que el arranque compila, salvo `examples/infer/tiny_*` (imprimen tiempos) y los que el arranque rechaza. Se mide con `selfhost_binaries_behave_like_bootstrap_binaries`.

Conformidad del lenguaje: `tests/lang/<área>/*.sal` lleva el resultado esperado escrito a mano (`# exit:`, `# stdout:`, `# error:`, `# xfail:` para huecos conocidos). `tests/lang.rs` lo comprueba con el arranque y `selfhost_meets_language_conformance` con el selfhost. `match` sobre enteros se baja en el emisor C del selfhost; enums, lambdas y `try` siguen pendientes. `selfhost_matches_bootstrap_on_every_repo_program` compara los dos compiladores sobre todos los `.sal` de `examples/`, `corpus/`, `tests/`, `std/` y `benches/` que el arranque compila. `tests/lang` ya no tiene huecos del arranque marcados con `# xfail:`. Enums con `match`, genéricos (tipos borrados a `Unknown`), lambdas (se inlinean en cada llamada), `try`/`Result`/`Option` (el preludio se añade si el programa los usa), `Float`, structs de varios campos, interpolación, propagación de efectos y `transpose` rank 2 estático (copia real vía `sal_transpose_f32`) funcionan en los dos compiladores. `# selfhost-xfail:` marca huecos solo del selfhost. Ya portados al selfhost y sin marca: `break` (`selfhost/loops.sal`, desugar a bucle con guardas como `src/loops.rs`), `method`/`static`/`init` de un solo módulo (`parser.sal` construye `Recv__nombre` con `self` implícito; `infer.sal` resuelve las llamadas con punto en modo de registro y `selfhost/methods.sal` reescribe el árbol), métodos y estáticos de los tipos básicos (`s.len()`, `xs.push(v)`, `List.new[T]()`, `Dict.new[K, V]()`, Float, Bytes) con retorno tipado, y los builtins de `Dict` en el emisor C (`dict_new` pasa los tipos de clave y valor al runtime). Métodos y estáticos entre módulos: `qualify` corre primero (renombra también `T__m` cuando su tipo choca entre módulos, `local_renames_methods`), y después `methods_resolve_graph` (en `modules.sal`) infiere cada módulo contra la unión de los ítems del grafo. `alias.f(..)` es una llamada a módulo aunque exista un local homónimo, y un `f` que no exporta el módulo es `E_TYPE`. Lambdas locales: la aridad se comprueba (`fn 1` en `infer.sal`). Tensores (todos los de `tests/lang/tensors` pasan): negación `-t` (`TensorUnary`), escalar a la izquierda (`s - t` como `(-t) + s`, como `src/ir.rs`), `relu`, `rms_norm`, `rebind`, `reduce` y `softmax` por ejes. Estos dos últimos y `relu` necesitan la forma del tensor en la emisión C: el lowering la pasa en la llamada IR como un slot extra que el texto IR no imprime (`ir_call_shaped`, `tensor_shape2`), y `tir.sal` la lleva en `Inst.n2` (`cir_softmax_axis`, `cir_reduce`, `cir_relu`). `relu` sobre una forma desconocida (`?`) es el mismo pánico en tiempo de ejecución que en el arranque. Divergencia SPEC↔arranque (resuelta a favor de la SPEC, l. 331: `alias.nombre` vale para llamadas, tipos y patrones de variante): el arranque acepta `alias.Variante(..)` y `alias.Variante` en patrones (`resolve_export_or_variant`, `qualify_pattern` conserva el calificador); los enums que solo llegan por un alias van a `TypeEnv.opaque_enums` (`register_signature_types`), así no se aplanan sus variantes sin cualificar. El selfhost cubre la construcción y el patrón cualificados (`push_variant_links_go`), el calificador desconocido y la variante inexistente (`qx_variant_check` en `qualify.sal`, que ahora recorre también los brazos de `match`), la variante sin cualificar de un enum que solo llega por alias (`alias_only_variant`, que no cuenta los módulos alcanzables por imports planos, directos y transitivos: `flat_keys`), y los tipos cualificados en firmas (`parse_type_text_dot`, `ts_trim` antes de cualificar). No hay huecos de conformidad del selfhost en `tests/lang`. `@deprecated("…")` y el aviso `W_DEPRECATED` están en el selfhost (`parser.sal`: `parse_attributed_item`, `TFn/TStruct/TEnum.deprecated` con prefijo "d:"; `deprecation.sal`: el pase, que corre en `emit_c_from_path` y `emit_c_for_single`). Ubicación de los avisos: el selfhost tiene spans para las llamadas (`XCall.xp`: byte del nombre; el `(` en una llamada con punto; el calificador en `alias.f`) y para los tipos (`TFn/TStruct/TEnum.tyat`: offsets separados por comas, en el orden en que el parser los lee; `deprecation.sal` los consume con un cursor). Línea y columna salen del texto preprocesado (`QDYN` se corrige por línea). `selfhost_deprecation_warnings_match_bootstrap` compara las líneas completas, con ubicación. Glue: el selfhost libera solo campos `String` de los structs (como antes); el arranque libera también `List`/`Dict`/`Bytes` planos y structs anidados, y esa regla aún no está portada (por eso `tyat` es un `String`).

Paridad medida por `tests/selfhost_ir.rs::selfhost_ir_matches_bootstrap_on_parity_files`:
el IR del selfhost coincide con el del arranque en todos los `.sal` del repositorio que el arranque
lowera (incluido el propio `selfhost/`).

Pendiente, por orden:

1. `infer` completo: formas de `attention`, `qmatmul`, `reduce`, `rms_norm` y `E_TENSOR_ELEM`, genéricos, enums,
   builtins con firma. (Hecho: `matmul`, `transpose`, `reshape`, `softmax`; structs de varios campos en el emisor C, como puntero a
   array de slots `int64_t`.)
   Structs por valor: el arranque los reparte en registros antes de emitir LLVM (`src/scalarize.rs`) y no cambia la IR de texto, así que la paridad no se ve afectada. El selfhost los sigue emitiendo como puntero a un array de slots; `p.x = v` copia antes de escribir, para que un struct se comporte como valor. Huecos del selfhost: un struct dentro de una `List` y como payload de un enum (`tests/lang/structs/struct_in_list.sal`, `struct_enum_payload.sal` llevan `# selfhost-xfail:`), y la copia en `a.b.c = v` anidado.
2. Los pases se aplican solo al compilar (`-o`), como `sal build`; `sal emit ir` no comprueba.
3. Lambdas y `try` en el bajado a IR. `match` sobre variantes ya baja a `EnumMake`/`EnumField`/`Switch` con el mismo texto que el arranque (`fuse.sal`, variantes de todo el grafo en `graph_types`); sigue sin bajar el `match` con patrones enteros (el arranque lo baja como cadena de `If`) ni `note_variant_binding`. Todos los `selfhost/*.sal` están en `PARITY_FILES`, más `corpus/enum_match.sal`.
4. Tipos de verdad en el AST (hoy texto), diagnósticos con spans. Avance: `tast.sal` construye el AST tipado (`to_typed`, `tp_free`) y `attrs`, `device`, `effects`, `ownership` e `infer` lo leen con `match`, y `drops` lo recorre y lo transforma (copias profundas `tc_*`; `drops_body_t` devuelve el cuerpo vía `fh_*`/`tt_*`); el tipado de nombres está en `ttypes.sal` y la tabla hash de cadenas de los modos de parámetros en `thash.sal`. `infer_param_modes` sigue recibiendo el AST de asas, lo convierte, calcula los modos y los escribe de vuelta. Con esto el pico de memoria de `stage1` al compilar `fuse.sal` y `emit.sal` bajó a unos 73 MB y 207 MB (antes unos 450 MB y 2,4 GB). Quedan sobre asas el parser, `qualify`, `modules`, `tensorops`, `ir`, `fuse` y `emit`. El backend C registra las variantes de todo el grafo, no solo las del módulo (antes un `match` sobre un enum importado nunca coincidía en stage2).
5. Backend LLVM / objeto, `--instrument`, kernels `on gpu kernel`.
6. Paquetes y `std` completos; `Result`/`Option` genéricos (el codegen del arranque falla con
   `match` de varias variantes: "duplicate case value in switch").

**Estado de la migración a tipos con dueño.** El selfhost ya compila enums con caja, structs de varios campos, `List[T]`/`Dict` y aplica el pase de drops tanto en el camino IR (`fuse.sal`) como en el AST→C (`emit.sal`, vía `drops_body_t`). El lexer y el estado del parser ya usan tipos con dueño: `Toks` y `Parser` son structs de `List[Int]`/`List[String]` (un token es su índice; `tok_kind/tok_text/tok_ival` reciben el parser; `parser_free` suelta las listas y la pila de sangrías se suelta en `lex`). Su propio AST sigue siendo asas `Int` (`vec_*`, `map_*`, `str_as_int`, ~280 `vec_new` y ~360 `str_as_int`); mientras sea así, el compilador autocompilado libera solo los locales simples y deja el resto vivo. Migrarlo exige reescribir parser, infer, ownership, effects, device, attrs, fuse, tensorops y emit sobre `enum Expr`/`enum Stmt`/`List[...]` El modo de parámetro (`borrow`/`take`) ya se infiere también en `selfhost/ownership.sal` (`infer_param_modes`, misma regla que `src/ownership.rs`) y lo usa `fuse.sal` para los drops de structs con `String`; el pase de drops calcula además los parámetros de solo lectura (`dr_read_only`, espejo de `read_only_params` en `src/drops.rs`), de modo que un valor pasado a una función que solo lo lee lo libera el llamante. La IR del corpus sigue idéntica en las tres cadenas (`corpus/param_modes.sal`, `corpus/drop_ro_params.sal`).

## Plan de migración del resto a tipos con dueño

Estado: `attrs`, `device`, `effects`, `ownership`, `infer`, `drops`, `tensorops`, `ir` y `fuse` (entrada) son tipados (`tast.sal`, `ttypes.sal`, `thash.sal`). El resto sigue en asas `Int`: `parser`, `qualify`, `modules`, `emit`, `ast.sal` (los tipadores de cadenas, que `emit` aún usa), `std/` y `standard/`.

No hay un primer paso independiente: `qualify` y `modules` mutan los programas de asas que guarda el grafo de módulos, y ese grafo lo consumen `fuse` y `emit`; `fuse` llama a `tensorops` y a los tipadores de `ast.sal`, que `emit` comparte; el parser produce las asas que todos leen. Cada módulo que se migre sin los vecinos necesita un puente (`to_typed` hacia delante, `fh_*` hacia atrás), y solo `drops` se pudo aislar así.

`tast.sal` ya tiene `fh_program` (programa tipado a asas) y `fh_roundtrip`. Comprobado: pasar cada programa que lee el parser por `fh_roundtrip` deja pasar todas las pruebas de `tests/selfhost_ir.rs` (IR idéntica en las tres cadenas y sin `LEAK` en el hello instrumentado), así que el puente de vuelta es fiel.

Orden propuesto, con un único puente que se va encogiendo:
1. Grafo de módulos tipado: `modules.sal` y `qualify.sal` guardan `TProgram` en una lista; el puente `fh_program` se coloca a la entrada de `fuse` y `emit`.
2. `tensorops.sal` y los tipadores de `ast.sal` (`expr_type`, `binary_add_*`, `call_type`) sobre `TEnv`, más `fuse.sal` y `ir.sal`. El contexto `cx` y las tablas de todo el grafo (`struct_map`, `fnrets`, `enums`) pasan a estructuras y `HT`.
3. `emit.sal` (camino AST y camino IR) y retirada del puente `fh_*`.
4. El parser produce `TProgram` directamente; se borran `ast.sal` y `to_typed`.
5. `std/` y `standard/` (unos 300 usos), y retirada de `vec_*`, `map_*`, `str_as_int`, `int_as_str` y `free` manual del lenguaje (`E_TYPE`).

Criterio por paso: IR idéntica en las tres cadenas (`tests/selfhost_ir.rs`), `selfhost_instrumented_clean_on_hello` sin `LEAK` y la memoria de `stage1` sobre `fuse.sal`/`emit.sal` no mayor que la medida (73 MB / 207 MB).

Avance del paso 1: `qualify.sal` califica el cuerpo de cada función sobre el árbol tipado
(`qx_*`, con `tt_block` al entrar y `fh_block` al salir); los tipos de parámetros y retorno, los
renombres de funciones y structs y la tabla de enlaces siguen en asas. Se conservan las
peculiaridades del pase anterior (solo desciende por campo, llamada, binario, `if`, `while`, `to`
y `on`; una llamada reconstruida conserva sus argumentos de tipo (antes los perdía, y `list_new[String]()` de un módulo calificado salía con el tipo de elemento 0) y un `on` sus banderas de kernel).
Paso 1 cerrado: el grafo es un `struct Graph { mods: List[Mod], progs: List[TProgram] }`
(`Mod { key, path, h }` es plano para que `list_get` lo copie entero; el programa tipado del módulo
`i` es `progs[i]`) y un módulo se nombra por su índice. `h` es la vista en asas que aún leen
`checks`, `fuse` y `emit`; `qualify` la modifica en su sitio y `refresh_typed` rehace `progs[i]`.
`resolve_module_graph*` llenan un `Graph` que crea el llamador. Memoria de stage1: 77 MB sobre
`fuse.sal` y 232 MB sobre `emit.sal` (antes 73/207), por tener el programa en las dos formas hasta
que el parser sea tipado (paso 4).

Avance del paso 2: `tir.sal` define el IR tipado (`Inst`, `FOp`, `Region`, `IrFn`, todos structs
planos de `String`, `Int` y `List` para que `list_get` los copie enteros) con el puente `ti_module`
desde las asas que produce `fuse` y `ti_free_module`. `ir_to_text` de `emit.sal` lo recorre tipado.
El recorrido `cir_*` (camino C) también lee el IR tipado (`ti_fn` por función, `Lit` para los
literales de tensor); los operandos se convierten a asa con `ih` (copia, 0 para el texto vacío)
porque las tablas de `cc` guardan las claves sin copiarlas. Con esto `emit.sal` ya no lee asas de
IR. Siguen en asas: `tensorops`, `ir.sal` (contexto `cx` y binds), `fuse.sal` (que produce las
asas de IR) y los tipadores de `ast.sal`.
Avance del paso 2 (entrada): `tensorops.sal`, `ir.sal` y `fuse.sal` leen `Expr`/`TBlock`/`TStmt` y tipan con `TEnv` (`texpr_type`, `tbinary_is_string_concat`, `tt_struct_find`) en lugar de `ex_kind`/`vec_get`/`strenv_*`. El contexto `cx` es ahora `struct Cx` (contador, instrucciones y acumulador en listas de una celda; `env: TEnv` compartido por todas las funciones del programa y truncado a 0 en cada una). `lower_program_with_types(prog, extra, env, enums)` recibe el `TEnv` del grafo (`tenv_for_graph` en `emit.sal`; `tenv_new` para un solo módulo) y convierte `prog` con `to_typed` tras `infer_param_modes`. `drops_body_t` es el único punto de entrada del pase de drops (ya no existe la versión sobre asas). Siguen en asas, por decisión de alcance: la IR que produce `fuse` (`ir_*`, binds, regiones, `vm` de variantes), `drop_env_from_prog` y los tipadores de `ast.sal` que `emit` comparte (`spine_acc`, `add_left_is_acc` conservan su forma de asas). `drops_body_t` devuelve el cuerpo vía `fh_block`/`tt_block` para que sus cajas no posean el texto (la IR lo guarda por asa hasta `ir_to_text`) y `lower_fn` pueda liberarlas.
Avance del paso 3 (emit AST→C): `emit_c_expr` y todo el camino AST→C de `emit.sal` (`emit_c_call`, `emit_c_match`, `emit_c_if`, `emit_c_stmts`, `where`, `matmul`, campos de struct) hacen `match` sobre `Expr`/`TBlock`/`TStmt`/`TArm`/`TPat` y tipan con `TEnv` (`texpr_type`, `texpr_is_string`, `tenv_bind/truncate`). El contexto es `struct Tc` (contador, acumulador y último `matmul` en listas de una celda; lambdas registradas en tres listas paralelas, `pool`, `env`); `emit_c_fn_body` obtiene el cuerpo con `drops_body_t` y lo suelta con `tp_free_block`. `emit_c_fns` recibe los `TFn` (`to_typed` tras bajar el módulo) y el `TEnv` del grafo. Siguen en asas: el pool de literales y variantes, `user`/`callables`, la cabecera de función y el camino `cir_*` (IR→C). `Tc` guarda el `TEnv` del llamante, de modo que `tc_new` lo declara `borrow`. Eso destapó un hueco del parser del selfhost: `borrow`/`take` tras el tipo de un parámetro entraban en el texto del tipo y el modo se perdía (`parse_type_text_ident` ya los deja fuera); lo cubre `tests/lang/ownership/param_mode_explicit.sal`. Medida: pico de `stage1` compilando `fuse.sal`/`emit.sal` ~1,26 GB / ~4,9 GB (tras el paso 2: 1,12 GB / 4,4 GB; antes del paso 2: 952 MB / 3,8 GB), es decir, peor que antes de migrar mientras `to_typed` y las asas de IR coexistan.
Avance del paso 4a (fuse): `lower_program_typed(tp, extra, env, em)` baja un `TProgram` ya convertido; `drop_env_t(tp)` arma variantes, modos efectivos (tabla de `own_mode_table`, explícitos incluidos) y solo-lectura sin pasar por asas. `infer_param_modes` (que escribía los modos en las asas) desaparece: los lectores (`mv_take_args`, `param_not_taken`) usan el vector de modos de `drop_env_t`. `emit_c_from_ast_module` convierte una sola vez y comparte el `TProgram` entre el entorno de drops, la bajada y la emisión. Paso 4a-2 (emit): el backend C recibe un `TProgram` (`emit_c_from_ast_module(tp, …, genums, env)`) y la lista de enums del grafo (`graph_enums_t`); registro de variantes, cabeceras, declaraciones adelantadas y `cir_fn` leen `TFn`/`TEnum`; `Tc` guarda los enums y `variant_field_type` busca `[índice de enum, índice de variante]`. Se borran `graph_types`, `prog_types`, `user_fns_from_prog`, `fnret_map_*` y el par fnrets/structs del pool. Solo `callable_fn_names`/`extern_fn_arity` y la entrada del IR (`emit_ir_from_graph`, que recibe el programa raíz como asa) siguen leyendo `h`. Memoria de stage1 sobre `fuse.sal`/`emit.sal`: 1,32 GB / 5,0 GB (sin mejora todavía).
Regla aprendida: un struct con `String` y con un campo struct anidado no se copia en `list_get`
(sin bits de tipo), así que su local libera dos veces; los elementos de lista deben ser planos.

Avance del paso 4b (qualify/modules): `qualify.sal` y `modules.sal` trabajan sobre `TProgram` (`Graph { mods, progs }`); ya no se lee ninguna asa de programa salvo al cargar.

Avance del paso 4c (parser): `parser.sal` construye `Expr`/`TBlock`/`TStmt`/`TFn`/`TStruct`/`TEnum`/`TProgram` directamente (`parse_program(src, errs) -> TProgram`; un error deja su mensaje en `errs`, se recoge con `errs_take`). Los textos que se guardan se copian (`tok_own`): los de listas de `String` los libera `list_free`. Los ids de sentencia salen de un contador del `Parser`. Se borran `ast.sal`, `to_typed`, `fh_*` y los `tt_*` de AST; los tipadores de texto que sobreviven están en `tytext.sal` y los ayudantes de asas de la IR en `tir.sal`. `drops_body_t` devuelve una copia de vista (`tv_block`): sus textos son copias que ninguna caja posee, de modo que `lower_fn` libera el árbol y la IR conserva los textos hasta `ir_to_text`. Los caminos de un solo fichero liberan el programa con `tp_free` (sin ello el hello instrumentado da `LEAK`). Memoria de stage1 sobre `fuse.sal`/`emit.sal`: 0,97 GB / 4,2 GB (antes 1,32 GB / 5,0 GB; sigue sobre los 73 MB / 207 MB de referencia, que aún no se recuperan).
