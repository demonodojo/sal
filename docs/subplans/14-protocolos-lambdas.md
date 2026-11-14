# Subplan: genéricos monomorfizados, lambdas como argumento y métodos con cuerpo en protocolos

La autoridad de la superficie es [SPEC.md](../../SPEC.md) («Genéricos», «Protocolos», «Métodos», gramática de `lambda`, `postfix` y `type`). Este subplan fija archivos, algoritmo y tests. Si diverge de la SPEC, manda la SPEC.

Objetivo: un protocolo puede llevar métodos con cuerpo que usan los requeridos (`Collection` con `map`, `reduce`, `each`…), cualquier tipo que lo implemente los recibe sin reescribirlos (también `List`, vía `impl List[T] Collection[T]` en el preludio), y una lambda se pasa a un método con la forma final de Ruby: `xs.map x => x * 2`, `xs.reduce(0) (acc, x) => acc + x`.

Las lambdas no son valores: cada llamada que recibe una se **especializa** (una copia de la función llamada con la lambda elevada a una `fn` propia; las capturas pasan como `&T`, o por valor si son `Copy`), como `impl Fn` en Rust. La misma maquinaria monomorfiza los genéricos.

## Fases

| Fase | Contenido | Estado |
|------|-----------|--------|
| 0 | Monomorfización de `fn` y `struct` genéricos, también entre módulos (SPEC «Genéricos») | Hecha en los dos compiladores |
| 1 | Tipo `fn(…) -> R`, lambdas de varios parámetros y con bloque, lambda final, especialización por lambda | Hecha en arranque y selfhost |
| 2 | Métodos con cuerpo (provistos) en protocolos | En curso |
| 3 | Protocolos genéricos, item `impl Tipo Protocolo`, `Collection` en el preludio | En curso |

**Estado de fase 1 (2026-11-13):** cerrada en ambos compiladores. El selfhost eleva las lambdas a
`__lamN`, crea copias `__sN`, propaga capturas compartidas sin emitir una referencia redundante,
conserva los tipos concretos de las copias genéricas y procesa el bloque elevado como bloque interno.
Los drops condicionales de parámetros `String` y structs también coinciden con el arranque.

Verificación de cierre: `selfhost_phase_one_lambda_ir_matches_bootstrap`,
`selfhost_conditional_string_param_drops_match_bootstrap` y `stage2_emit_ir_writes_selfhost_cache`
pasan; las pruebas de conformidad y memoria del arranque para la fase también pasan. La prueba
amplia `selfhost_meets_language_conformance` aún falla en varios programas genéricos con un
use-after-free de `TProgram` en `sp_rounds`; se reprodujo con un stage1 ASan anterior a los cambios
de esta divergencia, por lo que queda registrado como bloqueo previo de la fase 0, no como hueco de
lambdas. `selfhost_ir_matches_bootstrap_on_parity_files` también conserva una divergencia anterior
en `selfhost/ir/drops.sal` (cadena de `If` frente a `Switch`).

**Fase 2 (2026-11-13):** implementación parcial en el arranque. La SPEC y el AST admiten cuerpos,
`fmt` los conserva, el arranque comprueba el cuerpo con `self: any[P]` y sintetiza los métodos que
faltan después de desazucarar métodos. La conformidad del arranque cubre ejecución, override,
despacho por `any[P]` y errores de cuerpo/firma. `tests/selfhost_ir.rs` conserva
El selfhost sintetiza funciones para structs y enums, admite receptores por referencia, despacha
métodos provistos mediante `any[P]` y comprueba tipos y efectos del cuerpo con receptor abstracto
incluso sin implementadores. Los casos dirigidos de lambdas e imports pasan en stage1.
El arranque prepara los protocolos tras cualificar el grafo y copia al módulo del tipo los cuerpos
importados; `provided_imported_protocol.sal` cubre además un choque de nombres en el módulo receptor.
El selfhost también prepara los claims después de cualificar el grafo. La fase 2 sigue en curso:
la suite serial aún falla en la divergencia de IR de drops y en los genéricos, y LeakSanitizer
detecta fugas tanto en `hello` como en el compilador al procesar protocolos; hay que aislar las
introducidas por los cuerpos provistos antes de cerrarla.

**Fase 3:** los AST de ambos compiladores reservan ya los parámetros de protocolos y métodos; aún
no se ha habilitado su sintaxis ni implementado la resolución, las instancias o `Collection`.

## Fase 0: monomorfización

**Arranque.**
- `src/specialize.rs`:
  - `monomorphize` corre tras `methods::resolve_methods`: en un fichero suelto, al final de `methods::prepare_program`; en un grafo, en `modules::monomorphize_graph`, por módulo.
  - La inferencia registra cada llamada a una genérica en `TypeEnv.generic_calls` (`record_generic_call`: fn que llama, span de la llamada, tipos ligados o `None`) y devuelve el retorno con los tipos sustituidos.
  - El pase crea `g__<tipos>` (`instantiate`: firma, `let` y argumentos de tipo), renombra la llamada (`rename_call_block`) e itera hasta el punto fijo. Después quita las originales.
  - Las claves se recorren ordenadas por (fn que llama, inicio, fin). Cada copia se coloca tras su genérica y tras las copias anteriores de esa genérica.
  - Todo o nada por genérica: si una llamada no fija los tipos (`Outcome::Erase`), se empieza de nuevo con esa genérica borrada. Una genérica llamada desde otro módulo (`called_names`) queda borrada.
- Spans: cada copia recibe rangos `start`/`end` propios por encima del fuente (`shift_spans`, `span_base`; `line` y `col` no cambian, así que los diagnósticos se ubican igual). Las tablas span→tipo de la bajada (`StringEnv.types`, `drops::EXPR_TYPES`) son de todo el módulo; sin ese desplazamiento, dos copias de la misma genérica se pisaban los tipos.
- `infer_fn` comprueba el cuerpo de una genérica con sus parámetros de tipo borrados también dentro del cuerpo (`erase_body`: `List.new[T]()`, `let x: T`).

**Selfhost.**
- `selfhost/ast/tygeneric.sal`: ligar, sustituir y nombrar tipos de texto (`tg_bind`, `tg_subst`, `tg_mangle`, `tg_copy_name`).
- `selfhost/sem/infer.sal`: `IC.gcalls`, con las celdas `IC.cur` (fn) e `IC.xp` (offset de la llamada); `ic_record_generic` registra `fn \t offset \t genérica \t tipo…`.
- `selfhost/sem/specialize.sal`: `monomorphize` y `monomorphize_ext`. Ordena los registros como el arranque (`sp_sorted`: nombre bytewise y después offset), coloca las copias igual (`sp_place_copies`) y renombra por offset (`sp_rename_program`).
- Cableado: `parse_program_methods` (`parser.sal`) para un fichero suelto; `monomorphize_graph` (`modules.sal`, desde `resolve_module_graph_qualified` en `emit.sal`) para un grafo.

**Tests.**
- `tests/lang/generics/`: `generic_list_new.sal` (`List.new[T]()` dentro de la copia, dos instancias), `generic_calls_generic.sal` y `err_generic_instance_type.sal` (`E_TYPE` de la copia).
- Se quitan los `asan-xfail` de `generic_two_params.sal` y `generic_struct.sal`: antes fugaban 32 y 16 bytes, ahora salen limpios.
- `tests/forward_ir.rs` (`generic_fn_is_monomorphized_per_type`): nombres, orden y descriptor de cada copia.
- La paridad de IR del selfhost se mide sobre esos mismos programas.

**Entre módulos.**
- `MonoCtx` (`src/specialize.rs`; `selfhost/sem/mono_ctx.sal`) lleva:
  - las genéricas de los demás módulos (`externals`; en el selfhost, `ext`);
  - las copias que ya hizo el módulo que define cada genérica (`existing`, para llamarlas en vez de repetirlas);
  - los nombres ocupados en el grafo (`taken`);
  - las copias que el módulo hace de sus propias genéricas (`made`).
- Las originales no se quitan módulo a módulo. Las retira un barrido final hasta el punto fijo: `sweep_generics`; en el selfhost, `sp_sweep_generics`.
- Tests: `generic_cross_module.sal` y `generic_cross_module_alias.sal`, con `mods/generic_lib.sal`.

**Structs genéricos.**
- La inferencia admite `Box(d"a", 2)` con `struct Box[T]`: `infer_generic_struct_constructor`, que usa `specialize::bind_all`; en el selfhost, `infer_generic_struct`. Antes era «requires type arguments» aunque la SPEC lo permitía. Registra la construcción en `TypeEnv.generic_ctors` (`IC.gctors`).
- `monomorphize_structs` corre tras las funciones:
  - crea `Box__String` (`Instancer`; `StructMono` en `selfhost/sem/struct_mono.sal`);
  - reescribe los tipos de las funciones no genéricas, los structs, los enums y las constantes;
  - renombra las construcciones;
  - coloca cada instancia tras su genérico.
- El barrido (`sweep_generic_structs`; `sp_sweep_structs`) quita el genérico que ya nadie nombra.
- En el arranque, `Box[String]` y `Box__String` son el mismo tipo para `types_compatible` (`canonical_instance`, registro `INSTANCES`). La compatibilidad del selfhost ya es laxa con los tipos nominales.
- Tests: `tests/lang/structs/generic_struct_ctor.sal`, `generic_struct_nested.sal`, `generic_struct_through_fn.sal`, `err_generic_struct_unfixed.sal` y `tests/lang/generics/generic_struct_cross_module.sal` (con `mods/generic_box.sal`).

**Enums genéricos.**
- No se copian: tienen una sola representación.
- Un parámetro por valor de un enum genérico se libera: `owned_param_release`; en el selfhost, `dr_param_generic_kind` y `dr_param_caller_keeps`.
- El selfhost tipa el payload con los argumentos del escrutado (`infer_variant_field_ty`, `infer_payload_subst`). Antes valía `Unknown` y un método sobre él no se resolvía.
- Test: `tests/lang/enums/generic_enum_param_drop.sal`.

**Memoria.**
- Una `List` limpia (ligada una vez, llena de valores frescos o de parámetros por valor) que sale del ámbito por cualquier movimiento, también por `return`, se construye dueña de sus elementos: `moved_clean_list` / `dr_moved_clean_list`. Antes solo si iba a un constructor.
  - Es el modelo de Rust: quien la recibe suelta los `String`.
  - Cierra la fuga de `generic_list_new.sal`, que pierde su `asan-xfail`.
  - Destapó un uso tras liberar en `selfhost/sem/infer.sal` (`infer_elem` prestaba el elemento y luego soltaba la lista), ya corregido.
- `specialize.sal` consume cada `TProgram` por valor una sola vez en todas las ramas. Al compilar el selfhost, un programa que solo algunas ramas mueven no se suelta en las demás; ver «Pendiente».

**Pendiente.**
- El arranque no suelta un parámetro struct por valor que solo algunas ramas mueven. Ejemplo: `fn pick(p: Prog, q: Prog, first: Bool) -> Prog` devuelve uno y el otro se fuga. Necesita bandera de drop para structs en `src/drops.rs` y su espejo.
- El selfhost instrumentado da un `LEAK` en casi cualquier programa con una llamada a función de usuario. Igual que HEAD: es el problema de propiedad del árbol del selfhost, con el test `selfhost_instrumented_releases_rewritten_fn_bodies` ignorado.
