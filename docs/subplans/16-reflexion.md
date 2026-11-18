# Subplan: reflexión de structs (`Reflect`)

La autoridad de la superficie es [SPEC.md](../../SPEC.md) («Reflexión», en «Tipos»). Este subplan fija diseño, archivos, algoritmo y tests. Si diverge de la SPEC, manda la SPEC.

Objetivo: escribir código que reciba un struct sin conocer su tipo (por `any[Reflect]`), liste sus campos con sus tipos, lea un campo por nombre, lo escriba y lo vuelque a un diccionario. Casos de uso: serializar y deserializar (JSON), formularios, depuración y mapeos genéricos.

Decisiones del usuario (2026-11-17):

- Hay dos mecanismos y se hacen **por fases**: primero un protocolo `Reflect` con un enum `Value` en ejecución, y más adelante un bucle sobre los campos que se desenrolla al compilar.
- Se activa con la cláusula de protocolo que ya existe: `struct P impl Reflect`. No hay atributo nuevo ni reflexión automática en todos los structs.
- Escribir un campo con un `Value` de otro tipo, o un campo que no existe, devuelve `Err` (`Result[Unit, String]`). No hay pánico.

## Fases

| Fase | Contenido | Estado |
|------|-----------|--------|
| 0 | Requisitos previos: fallos de `any[P]`, de `match` y de protocolos que impiden escribir `Reflect`; literal `()` | Hecha (2026-11-17), arranque y selfhost |
| 1 | `std/reflect.sal` (`Reflect`, `Value`, `Field`) y síntesis de los métodos para cada `struct T impl Reflect` | Hecha (2026-11-18), arranque y selfhost; sin structs genéricos |
| 2 | `from_dict` estático (construir un `T` desde un `Dict[String, Value]`) | Hecha (2026-11-18), arranque y selfhost |
| 3 | Bucle en compilación sobre los campos (`inline for f in T.fields`, `x.[f]`) | Hecha (2026-11-18), arranque y selfhost; sin `Self` ni structs importados |

Cada fase se cierra en arranque **y** selfhost.

## Fase 0: requisitos previos

Encontrados el 2026-11-17 al escribir a mano, con `sal` 0.6.1, lo que generaría la fase 1. Los programas mínimos están más abajo. Todos están arreglados, con su test en `tests/lang` y sin marca `xfail`:

| Fallo | Test |
|-------|------|
| 1, 2 | `protocols/any_struct_fields_run.sal`, `any_let_owned_value.sal`, `any_assign_and_field.sal`, `any_return_statement.sal`, `err_any_borrowed_let.sal`, `err_any_self_by_value.sal` |
| 3 | `protocols/protocol_names_prelude_option.sal` |
| 4 | `control_flow/match_nested_same_outer.sal`, `match_nested_refutable_falls_through.sal` |
| 5 | `protocols/any_enum_payload.sal` (el payload de una variante es uno de los sitios donde se coacciona, SPEC «Protocolos») |
| 6 | `types/unit_literal.sal`, `types/err_unit_literal_as_int.sal` |

La lista original:

1. **`any[P]` de un struct de dos o más campos no compila.** `p = P(1, 2)` seguido de `use(&p)`, con `use(o: &any[R])`, da `E_INTERNAL`: el LLVM usa un valor sin definir en `sal_any_box(…, i64 2, i64 %t3)`. La caja no se construye a partir de los registros del struct repartido (ver «struct … se reparte en un registro por campo» en la SPEC). El único test que ejecuta código por `any` (`tests/lang/protocols/any_show_run.sal`) usa un struct de un campo.
2. **`let b: any[R] = P(41)` devuelve un valor erróneo sin avisar.** Después, `use(&b)` llama a `o.n()` y obtiene `0` en vez de `42`; con `P(1, 2)` se queda en `0` en vez de `3`, y un `String` devuelto por la vtable sale vacío. Si el método devuelve una lista, `sal --sanitize` da `SEGV` en `sal_any_call` (lee `0xbebe…`, memoria sin inicializar). Coaccionar en la llamada (`use(&p)`) funciona con un campo.
3. **Un protocolo del módulo raíz cuya firma nombra `Option` rompe el preludio.** Con `import "std/prelude.sal"` y `protocol R` / `method get_field(&self, name: &String) -> Option[Int] ! alloc`, `sal check` falla con `E_TYPE` «dict_get requires Option…» en `std/prelude.sal:98`. Sin el protocolo, compila.
4. **Varios brazos con el mismo constructor externo y patrones anidados** (`Some(VInt(n))`, `Some(VStr(s))` en un mismo `match`) dan `E_INTERNAL`: «duplicate case value in switch» en LLVM. Tienen que funcionar o ser un error con su código.
5. **El payload de una variante no coacciona a `any[P]`.** `VStruct(self.pos.clone())`, con `VStruct(any[Reflect])`, es `E_TYPE` «expected any, found Punto». La SPEC dice que un valor se mueve a la caja «donde se espera `any[P]`»; hay que decidir si el argumento de una variante es uno de esos sitios y fijarlo en la SPEC. La fase 1 no lo necesita si los structs anidados se reflejan como `VDict`, como se propone abajo.
6. **No hay valor de `Unit`.** `Ok(())` es `E_PARSE`, así que una función `-> Result[Unit, String]` no puede devolver `Ok`. Se propone añadir el literal `()` de tipo `Unit`, como en Rust: gramática de `primary` en la SPEC, parser, `fmt` e inferencia en los dos compiladores. Es un cambio de lenguaje propio, con su test.

Una nota de redacción: la SPEC dice que `return` vale «anidado» en `match`. `arm_body → expr` no admite `return` en línea (`_ => return Err(…)` es `E_PARSE`), pero sí en un brazo con bloque. Ese brazo tipaba el `match` con su `Unit`, en contra de «una rama que retorna no aporta valor»; se cerró en `infer.rs` con `control_flow/match_arm_return.sal`.

## Fase 1: `Reflect`

### Superficie

Módulo nuevo `std/reflect.sal`, fuera del preludio, para que `Value` y `Field` no choquen con los nombres de quien no lo importa:

```sal
enum Value
    VInt(Int)
    VFloat(Float)
    VBool(Bool)
    VStr(String)
    VList(List[Value])
    VDict(Dict[String, Value])

struct Field
    name: String
    ty: String           # texto canónico del tipo: "Int", "List[String]", "Punto"

protocol Reflect
    method type_name(&self) -> String ! alloc
    method fields(&self) -> List[Field] ! alloc
    method get_field(&self, name: &String) -> Option[Value] ! alloc
    method set_field(&mut self, name: &String, v: Value) -> Result[Unit, String] ! alloc
    method to_dict(&self) -> Dict[String, Value] ! alloc
```

Los nombres son `get_field` y `set_field`, no `get` y `set`, para que un struct pueda cumplir a la vez `Reflect` y `Collection`.

Uso:

```sal
import "std/reflect.sal"

struct Persona impl Reflect
    nombre: String
    edad: Int
    pos: Punto           # Punto también impl Reflect

fn volcar(x: &any[Reflect]) -> Unit ! io, alloc
    fs = x.fields()
    i = 0
    while i < fs.len()
        f = fs.get(i)
        print(f.name + ": " + f.ty)
        i = i + 1

fn main() -> Int ! io, alloc
    p = Persona(d"Ana", 30, Punto(1, 2))
    p.set_field(&d"pos.x", VInt(5))      # nombre con puntos: entra en el campo anidado
    volcar(&p)
    0
```

### Reglas (diseño previo; las que valen son las de la SPEC «Reflexión»)

Diferencias entre este diseño y lo implementado: no se exige la forma de `Value` y `Field` (los métodos escritos nombran los de `std/reflect.sal`, y sin el import son `E_TYPE`); los structs genéricos y los enums con `impl Reflect` son `E_TYPE` en este corte; un campo struct de otro módulo se acepta y tiene que cumplir `Reflect` él mismo; y los contenedores de contenedores son `E_TYPE`.

- El compilador reconoce el protocolo por su nombre, `Reflect`, igual que reconoce `Option` para `dict_get`, y exige que `Value` y `Field` tengan la forma de `std/reflect.sal`. Si no la tienen, `E_TYPE`.
- Para un `struct T impl Reflect`, el compilador sintetiza los cinco métodos que `T` no defina él mismo, en el módulo de `T`. Un método propio prevalece si su firma coincide, como un método provisto.
- **Campos admitidos** y su `Value`: `Int`, `Float`, `Bool`, `String`, `List[E]` con `E` admitido (→ `VList`), `Dict[String, V]` con `V` admitido (→ `VDict`), y un struct que también es `impl Reflect` (→ `VDict` con su `to_dict`). Cualquier otro campo (referencias, `Tensor`, `any[P]`, enums, asas de hilo, `Bytes`, structs sin `Reflect`) hace que `impl Reflect` sea `E_TYPE` en el span del campo, con el motivo.
- **Memoria (Rust):** `get_field` y `to_dict` clonan: el `Value` es dueño y no presta de `self` (de ahí el efecto `alloc`). `set_field` mueve `v`; el valor anterior del campo se suelta con la glue de su tipo. No hay alias implícitos.
- **Nombres con puntos:** `get_field(&"pos.x")` y `set_field(&"pos.x", v)` separan el nombre por el primer `.` y delegan en el método del campo, que tiene que ser un struct `Reflect`. Así se escribe un campo anidado sin tener que convertir `any` a su tipo (que sal no tiene).
- **Errores de `set_field`** (`Err` con el texto, sin `panic`): campo inexistente (`"no field x in Persona"`), `Value` de otra variante (`"field edad: expected Int, found String"`), y una lista o un diccionario con algún elemento de otro tipo. Un struct anidado se escribe con un `VDict`: campo a campo, y si falla uno queda el valor anterior entero (primero se construye y después se mueve).
- `fields()` sigue el orden de declaración. `ty` es el tipo como lo escribe `sal fmt`, con los tipos ya fijados en un struct monomorfizado.
- Structs genéricos: `struct Box[T] impl Reflect` sintetiza en cada copia monomorfizada (`Box__Int`), después de monomorfizar, y comprueba ahí los campos admitidos. `type_name()` devuelve `"Box[Int]"`.
- Un struct `impl Reflect` puede ir en `any[Reflect]` como cualquier protocolo (vtable con los cinco métodos y el drop).

### Algoritmo y archivos (lo implementado)

Los métodos se generan **como texto sal** y se añaden al final del fuente del módulo antes del lexer. Así pasan por todos los pases como código escrito a mano (tipos, préstamos, drops, IR), con spans propios y sin reglas nuevas en ningún pase. El texto es el mismo en los dos compiladores, y por eso también la IR.

| Compilador | Archivo | Cambio |
|------------|---------|--------|
| arranque | `src/reflect.rs` | `with_reflect(src)`: busca las cabeceras `struct … impl …, Reflect`, lee los campos (`name: Tipo`), los clasifica (`parse_ty`) y escribe los métodos que el struct no define (`has_own`), más funciones auxiliares `reflect_<T>_<campo>_{value,from,entry}` para listas y dicts. `reflect_error(src)` da el `E_TYPE` de un campo no admitido o de un struct genérico o enum. Lo llaman `parse_program` y la carga de módulos |
| selfhost | `selfhost/front/reflect/reflect.sal`, `reflect_header.sal`, `reflect_struct.sal` | Espejo de `src/reflect.rs`, con el mismo texto; `selfhost/front/parser.sal` llama a `reflect_error` antes de parsear |
| std | `std/reflect.sal` | `Value`, `Field`, `Reflect` y las funciones que usa el texto generado (`value_kind`, `value_mismatch`, `value_field`, `value_in`…) |
| SPEC | «Reflexión» | Las reglas |

Arreglos de memoria y de tipos que salieron al probarlo con `--sanitize` (los dos compiladores, cada uno con su test en `tests/lang/reflect`):

- La caja de un enum guarda con glue un struct describible (`Ok(field)`) y un `Dict` (`struct_enum_glue`, `sal_dict_drop_glue`); un payload genérico toma el tipo del valor que lo llena (`payload_field_type` / `payload_field_text`).
- `print(x)` presta, como `println!`; sus temporales se sueltan (`borrows_args`, `reads_string_arg`).
- El resultado de una función de usuario que se usa como operando de `+` o como receptor de `len` se liga a un temporal y se suelta (`owned_operand`, `user_container_call`).
- Reasignar un campo struct anidado de dos o más campos suelta el bloque viejo (`emit_flat_block_drop` / `lower_field_drop_flat`).
- El selfhost suelta un `Dict[K, Enum]` o `Dict[K, Struct]` local (`dx_val_owned`) y tipa la llamada a un método por `any[P]` (`tenv_fill_protocols`, `tt_any_call`); sin eso, `x.fields().len()` por `any` bajaba `+` de `String` como suma de enteros.

### Huecos conocidos

Ninguno de estos tiene marca en `tests/lang/reflect` porque los tests no los ejercitan; están anotados para no tropezar con ellos al usar `Reflect`:

- El selfhost tipa un `match` como `Unknown`: llamar un método sobre un local ligado a un `match` necesita anotarlo (`let antes: Int = match …`).
- La IR de los programas con `any[P]` difiere entre compiladores (thunks y orden de temporales); el resultado y el código de salida coinciden.
- Un `&d"…"` temporal dentro de un brazo de `match` usado como expresión fuga: el hoist de temporales no entra en los brazos.
- `owned_return_fns` es conservador: una función con un `while` en el nivel superior o con un local religado no consta como dueña de su `String` devuelto, y el llamador fuga.
- `try` y mover fuera de un enum fugan la cáscara de la caja.
- El selfhost solo sigue los parámetros `any[P]`, no los locales.

### Tests

- `tests/lang/reflect/` (área nueva, que se añade a la lista de `tests/memory_asan.rs`):
  - `fields_run.sal`: `fields()` de un struct con `Int`, `String` y `List[Int]`; exit y stdout exactos.
  - `get_set_run.sal`: `get_field`/`set_field` correctos, y `Err` por nombre y por tipo, con el texto exacto.
  - `nested_dotted.sal`: `"pos.x"` en get y set.
  - `to_dict_run.sal`: `to_dict` con un struct anidado.
  - `through_any.sal`: todo lo anterior por `&any[Reflect]` con un struct de varios campos (depende de la fase 0).
  - `set_struct_and_lists.sal`: `set_field` de un struct anidado, de listas y de dicts, con el error de un elemento.
  - `own_method_wins.sal`: un `type_name` propio prevalece.
  - `from_dict_run.sal`: ida y vuelta, campo que falta y campo de otro tipo.
  - `err_field_tensor.sal`, `err_field_ref.sal`, `err_field_no_reflect.sal`, `err_list_of_lists.sal`, `err_generic_reflect.sal`: `E_TYPE`.
- Sin fugas con `--sanitize`: get, set (que suelta el valor anterior), to_dict, y set fallido (que suelta el `Value` que se le pasó).
- `selfhost_meets_language_conformance` y la paridad de IR en `tests/selfhost_ir.rs`.

## Fase 2: `from_dict`

`static from_dict(d: &Dict[String, Value]) -> Result[Self, String] ! alloc` en `Reflect`. Como es un static, no está en la vtable ni se llama por `any` (SPEC «Protocolos»): se llama sobre el tipo concreto, `Persona.from_dict(&d)`. Junto con `to_dict`, da serialización de ida y vuelta (`std/json` podrá construir un `Value` y de ahí un struct). Faltar un campo o tener uno de otro tipo es `Err`.

## Fase 3: bucle en compilación

Recorrer los campos con su tipo real, sin `Value` ni cajas, al estilo de `inline for` de Zig. Reglas en la SPEC («Reflexión», `inline for`).

### Diseño

- Gramática: `line → "inline" "for" IDENT "in" IDENT "." "fields" suite` y `postfix → postfix "." "[" IDENT "]"`. `inline`, `for` y `fields` son contextuales y `in` ya era palabra clave (`kernel`): no hay palabras reservadas nuevas.
- Dentro del cuerpo, `f.name` y `f.ty` son literales `STRING`, y `x.[f]` es `x.<campo>`. Así un campo se lee, se presta y se asigna con las reglas de siempre, sin pasar por `Value`.
- Desenrollado **antes de inferir**, como `desugar_break`: ningún pase posterior ve el nodo. Cada copia es la rama de un `if true` (sus locales no chocan entre campos y se sueltan al final, como en cualquier rama; un bloque suelto como sentencia no soltaba sus locales) y se tipa con su campo.
- Cada copia tiene posiciones propias: el arranque desplaza sus spans (`k · stride`, como `protocols.rs`), y el selfhost desplaza el offset de cada llamada (`k · (len + 1)`) y el id de cada sentencia (`k · 2^32`). Sin eso, `specialize` y `drops` confundirían las llamadas y sentencias de copias distintas. `deprecation.sal` toma el offset módulo `len + 1` para situar el aviso.

| Compilador | Archivo | Cambio |
|------------|---------|--------|
| arranque | `src/ast.rs` | `Stmt::InlineFor { var, ty, body, span }`; `x.[f]` es un `Expr::Field` llamado `[f]` |
| arranque | `src/parser.rs`, `src/fmt.rs` | La línea `inline for` y el postfijo `.[f]`; `fmt` los escribe igual |
| arranque | `src/inline_for.rs` | `unroll_inline_for(prog)`, llamado justo antes de `desugar_break` en `compile.rs`, `modules.rs` y `main.rs` |
| selfhost | `selfhost/front/parser.sal` | Sentencia de `kind` 5 (variable en `name`, struct en `ty`, cuerpo en `XBlock`) y postfijo `.[f]` (`XField` llamado `[f]`); `desugar_inline` antes de `desugar_break` |
| selfhost | `selfhost/front/inline_for.sal` | Espejo de `src/inline_for.rs` |

### Corte

Fuera de este corte, y por eso `E_TYPE`: `Self` en un protocolo (el cuerpo provisto se comprueba una vez, con `Self` abstracto; haría falta desenrollar después de copiar el cuerpo a cada tipo), structs importados (el pase solo ve el módulo) y structs genéricos (los campos no tienen tipo fijo antes de monomorfizar). Tampoco hay `if` en compilación: el cuerpo tiene que tipar para todos los campos.

### Tests

`tests/lang/reflect/inline_for_*.sal` (suma, nombres y tipos, asignación por `&mut`, locales `String` por copia, `return`) y `err_inline_for_*.sal` (enum, genérico, variable suelta, campo de otro tipo, `.[f]` fuera, `T.campos` es `E_PARSE`, protocolo, `break`); `fmt_inline_for_and_field_of_loop_var` en `tests/parse_and_fmt.rs`.

## Reproducciones de la fase 0

```sal
# 1 y 2: any[P] con struct de dos campos; con let, valor erróneo
protocol R
    method n(&self) -> Int

struct P impl R
    x: Int
    y: Int

method P.n(&self) -> Int
    self.x + self.y

fn use(o: &any[R]) -> Int
    o.n()

fn main() -> Int ! alloc
    p = P(1, 2)
    use(&p)            # E_INTERNAL (%t3 sin definir); con `let b: any[R] = P(1, 2)` y use(&b): 0, no 3
```

```sal
# 3: protocolo del raíz que nombra Option
import "std/prelude.sal"

protocol R
    method get_field(&self, name: &String) -> Option[Int] ! alloc

fn main() -> Int
    0                  # E_TYPE en std/prelude.sal:98 (dict_get requires Option…)
```

```sal
# 4: patrones anidados con el mismo constructor externo
match x.get_field(&f.name)
    Some(VInt(n)) => …
    Some(VStr(s)) => …   # E_INTERNAL: duplicate case value in switch
    _ => …
```
