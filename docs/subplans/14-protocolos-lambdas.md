# Subplan: genéricos monomorfizados, lambdas como argumento y métodos con cuerpo en protocolos

La autoridad de la superficie es [SPEC.md](../../SPEC.md) («Genéricos», «Protocolos», «Métodos», gramática de `lambda`, `postfix` y `type`). Este subplan fija archivos, algoritmo y tests. Si diverge de la SPEC, manda la SPEC.

Objetivo: un protocolo puede llevar métodos con cuerpo que usan los requeridos (`Collection` con `map`, `reduce`, `each`…), cualquier tipo que lo implemente los recibe sin reescribirlos (también `List`, vía `impl List[T] Collection[T]` en el preludio), y una lambda se pasa a un método con la forma final de Ruby: `xs.map x => x * 2`, `xs.reduce(0) (acc, x) => acc + x`.

Las lambdas no son valores: cada llamada que recibe una se **especializa** (una copia de la función llamada con la lambda elevada a una `fn` propia; las capturas pasan como `&T`, o por valor si son `Copy`), como `impl Fn` en Rust. La misma maquinaria monomorfiza los genéricos.

## Fases

| Fase | Contenido | Estado |
|------|-----------|--------|
| 0 | Monomorfización de `fn` genéricas (SPEC «Genéricos») | Hecha en los dos compiladores |
| 1 | Tipo `fn(…) -> R`, lambdas de varios parámetros y con bloque, lambda final, especialización por lambda | Pendiente |
| 2 | Métodos con cuerpo (provistos) en protocolos | Pendiente |
| 3 | Protocolos genéricos, item `impl Tipo Protocolo`, `Collection` en el preludio | Pendiente |

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

**Pendiente.**
- Una genérica llamada desde otro módulo sigue borrada: sus copias tendrían que vivir en el módulo que llama, con las firmas del módulo que la define a la vista.
- Los `struct` y `enum` genéricos no se monomorfizan.
- El stage1 instrumentado que compila un programa con genéricas da un `LEAK` de `enum_new`: un nodo del parser (`parse_primary`, `parse_call_with_type_args`) que se pierde al soltar por la glue el `TProgram` original una vez copiado. HEAD ya fugaba una caja igual con `generic_two_params.sal`. `selfhost_instrumented_clean_on_hello` no lo ve, porque el hola mundo no tiene genéricas. Por eso `specialize.sal` copia campo a campo (`sp_union`) en vez de usar el `clone` del struct entero, que fugaba además al compilar el hola mundo.
- `generic_list_new.sal` lleva `asan-xfail`: una `List[String]` llenada con `String` recibidos por parámetro y devuelta no suelta sus elementos. Pasa igual sin genéricos.
