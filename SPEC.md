# Especificación del lenguaje sal (arranque)

La gramática de este documento es la definición del lenguaje. Es recursiva por la izquierda en las listas, en los operadores binarios y en los postfijos. Esa recursión es la asociatividad: la derivación izquierda de `1 - 2 - 3` es `(1 - 2) - 3`, la de `a.b.c` es `(a.b).c` y la de `f(x)(y)` es `(f(x))(y)`.

Los prefijos (`try`, `-`, `!`, `to`, `&`, `&mut`, `*`) y la lambda son recursivos por la derecha. `x => y => z` es `x => (y => z)` y `- - x` es `- (- x)`. Una reescritura recursiva por la derecha de los operadores no es esta gramática: cambiaría el árbol.

El lexer parte el fuente en terminales e inserta `NEWLINE`, `INDENT` y `DEDENT`. La sangría son espacios; un tabulador es `E_PARSE`. Una línea en blanco no abre un bloque. Entre líneas el parser acepta uno o más `NEWLINE`. Los comentarios `#` desaparecen en el lexer y no tienen producción.

Mientras haya un `(` o un `[` sin cerrar, un salto de línea emite `NEWLINE` y no inserta `INDENT` ni `DEDENT`. En ese tramo el parser ignora `NEWLINE` antes de un operador, un operando, una coma o el cierre. La expresión sigue siendo una: `1 - 2 - 3` partido dentro de paréntesis es el mismo árbol que en una línea. Fuera de `(…)` y `[…]`, el salto de línea sigue terminando la sentencia.

## Superficie

- Indentación significativa; comentarios `#`.
- Bloque: la última expresión es el valor en los caminos que llegan al final; `return expr` o `return` termina la función en ese punto.
- `fn nombre(params) -> Tipo ! efectos` con efectos `io`, `alloc`, `panic`, `gpu`, `tpu`.

## Gramática

Los no terminales van en minúsculas. `ε` es la producción vacía. `IDENT`, `INT`, `FLOAT`, `STRING`, `DSTRING` y `FSTRING` son terminales del lexer. Lo demás entre comillas es terminal.

### Programa

```text
program      → item_list
item_list    → item_list item
             | ε
item         → import_def
             | attr_list struct_def
             | enum_def
             | frame_def
             | protocol_def
             | attr_list fn_def
             | attr_list method_def
             | attr_list static_def
             | attr_list init_def
             | attr_list const_def
attr_list    → attr_list attr
             | ε
attr         → "@" IDENT ("(" IDENT ")" | ε)

import_def   → "import" path import_as
import_as    → "as" IDENT
             | ε
path         → path "." IDENT
             | IDENT
             | STRING

struct_def   → "struct" IDENT type_params impl_clause suite_fields
suite_fields → NEWLINE INDENT field_list DEDENT
field_list   → field_list field
             | field
field        → IDENT ":" type

frame_def    → "frame" IDENT type_params suite_columns
suite_columns → NEWLINE INDENT column_list DEDENT
column_list  → column_list column
             | column
column       → IDENT ":" column_type
column_type  → "F32" | "F16" | "BF16" | "I8" | "String"

enum_def     → "enum" IDENT type_params impl_clause NEWLINE INDENT variant_list DEDENT
impl_clause  → "impl" protocol_list
             | ε
protocol_list → protocol_list "," IDENT
              | IDENT
protocol_def → "protocol" IDENT NEWLINE INDENT protocol_method_list DEDENT
protocol_method_list → protocol_method_list protocol_method_sig
                     | protocol_method_sig
protocol_method_sig → "method" IDENT "(" method_params ")" "->" type effect_clause NEWLINE
variant_list → variant_list variant
             | variant
variant      → IDENT variant_payload
variant_payload → "(" type_list ")"
                | ε
type_list    → type_list "," type
             | type
             | ε

fn_def       → "fn" IDENT type_params "(" param_list ")" "->" type effect_clause suite
method_def   → "method" IDENT "." IDENT "(" method_params ")" "->" type effect_clause suite
method_params → "&" "mut" "self" "," param_list
              | "&" "self" "," param_list
              | "&" "mut" "self"
              | "&" "self"
              | "self" "," param_list
              | "self"
              | param_list
static_def   → "static" IDENT "." IDENT "(" param_list ")" "->" type effect_clause suite
init_def     → "init" IDENT "(" param_list ")" effect_clause suite
const_def    → IDENT (":" type | ε) "=" expr
suite        → NEWLINE INDENT block DEDENT
type_params  → "[" type_param_list "]"
             | ε
type_param_list → type_param_list "," type_param
                | type_param
type_param   → "'" IDENT
             | IDENT
param_list   → param_list "," param
             | param
             | ε
param        → IDENT ":" type
effect_clause → "!" effect_list
              | ε
effect_list  → effect_list "," effect
             | effect
effect       → "io" | "alloc" | "panic" | "gpu" | "tpu"
```

### Bloque

```text
block        → line_list
line_list    → line_list line
             | ε
line         → attr_list "let" IDENT (":" type | ε) "=" expr
             | "return" (expr | ε)
             | "break"
             | "while" expr suite
             | expr ("=" expr | ε)
```

Si hay `=`, la izquierda tiene que ser un lvalue: `IDENT`, un postfijo que solo sea una cadena de campos, o un prefijo `*` (escribir a través de una referencia). Si no lo es, `E_PARSE`. La última línea que es una expresión sin `=` es el valor del bloque en los caminos que no terminan con `return`. Sin esa línea el bloque vale `Unit`; si la función no devuelve `Unit`, el error es `E_TYPE`.

`return expr` termina la función con `expr`; el tipo de `expr` tiene que ser el de la función o `E_TYPE`. `return` sin expresión solo si la función devuelve `Unit`. Vale en el cuerpo de la función y, anidado, en `if`, `elsif`, `else`, `while` y `match`. Dentro de `on` o de una lambda es `E_TYPE`. Tras un `return` que siempre se alcanza en el mismo bloque (otra sentencia o la cola) es `E_TYPE`. Un `while` no garantiza el retorno; un `if` sin `else` tampoco. Una rama que retorna no aporta valor al `if`/`match`; el tipo es el de las ramas que sí producen valor. Si todas las ramas retornan, la expresión diverge.

`break` sale del `while` más interno. Es una sentencia del cuerpo de un `while` o de un bloque `if` / `elsif` / `else` dentro de él; en cualquier otro sitio (fuera de un bucle, en un brazo de `match`, en `on`, en una lambda, en el valor de un `let`…) es `E_TYPE`, y también lo es el código que le sigue en el mismo bloque. Tras un `break` la condición del `while` no se vuelve a evaluar. El arranque lo reescribe antes de inferir tipos con una bandera local (`__brkN`); el `fmt` conserva `break`.

### Expresiones

```text
expr         → "try" expr
             | lambda
lambda       → or "=>" expr
             | or
or           → or "||" and
             | and
and          → and "&&" cmp
             | cmp
cmp          → cmp "==" bor
             | cmp "!=" bor
             | cmp "<"  bor
             | cmp "<=" bor
             | cmp ">"  bor
             | cmp ">=" bor
             | bor
bor          → bor "|" bxor
             | bxor
bxor         → bxor "^" band
             | band
band         → band "&" shift
             | shift
shift        → shift "<<" add
             | shift ">>" add
             | add
add          → add "+" mul
             | add "-" mul
             | mul
mul          → mul "*" unary
             | mul "/" unary
             | mul "%" unary
             | unary
unary        → "-" unary
             | "!" unary
             | "to" place unary
             | "&" "mut" unary
             | "&" unary
             | "*" unary
             | postfix
postfix      → postfix "(" arg_list ")"
             | postfix "[" type_arg_list "]" "(" arg_list ")"
             | postfix "." IDENT
             | primary
arg_list     → arg_list "," expr
             | expr
             | ε
type_arg_list → type_arg_list "," type_arg
              | type_arg
type_arg     → type
             | INT
             | "?"
primary      → INT
             | FLOAT
             | "true"
             | "false"
             | STRING
             | DSTRING
             | FSTRING
             | IDENT
             | "(" expr ")"
             | tensor_lit
             | on_expr
             | match_expr
             | if_expr

tensor_lit   → "tensor" "[" row_list "]"
row_list     → row_list "," row
             | row
row          → "[" elem_list "]"
elem_list    → elem_list "," expr
             | expr
             | ε

on_expr      → "on" place kernel_head suite
kernel_head  → ε | "kernel" kernel_spec | "kernel"
kernel_spec  → IDENT ("," IDENT)* "in" type
match_expr   → "match" expr NEWLINE INDENT arm_list DEDENT
arm_list     → arm_list arm
             | arm
arm          → pattern "=>" arm_body
arm_body     → expr
             | suite
pattern      → "_"
             | INT
             | STRING
             | RSTRING
             | IDENT "." IDENT "(" pattern_list ")"
             | IDENT "(" pattern_list ")"
             | IDENT
pattern_list → pattern_list "," pattern
             | pattern
             | ε
if_expr      → "if" expr suite elsif_list ("else" suite | ε)
elsif_list   → elsif_list "elsif" expr suite
             | ε
```

`&&` y `||` son los operadores lógicos. Toman valores de verdad: `Bool`, o `Int` donde distinto de cero es verdadero (como la condición de `if` y `while`), y devuelven `Bool`. Cualquier otro tipo es `E_TYPE`. Cortocircuitan: `a && b` no evalúa `b` si `a` es falso, y `a || b` no evalúa `b` si `a` es verdadero. El valor es siempre `true` o `false`, aunque el operando sea `Int`. Una comparación escalar sigue valiendo `Int` (`0` o `1`).


El parámetro a la izquierda de `=>` en `lambda` es un `IDENT`. `a + b => c` es `E_PARSE`. Dentro de `match`, `=>` pertenece a `arm`, no a `lambda`: `Some(x) =>` no se parsea como lambda. Tras `=>`, un `arm_body` puede ser una expresión o un bloque indentado (`suite`); el bloque sigue las mismas reglas de valor, `return` y `break` que el cuerpo de un `if`.

Un `match` sobre un escalar distinto de enum elige el **primer** brazo cuyo patrón coincide. `_` coincide siempre. Un `INT` exige escrutado `Int` y `==`. Un `STRING` en el patrón exige escrutado `String` e igualdad exacta (`str_eq`). Un `RSTRING` exige escrutado `String` y encaja si la expresión regular POSIX ERE (como `regexec`, sin anclar salvo `^`/`$` en el patrón) coincide en algún punto; no liga grupos. Mezclar patrones de enteros, cadenas o regex con patrones de variantes de enum en el mismo `match` es `E_TYPE`. Mezclar enteros con cadenas o regex es `E_TYPE`. Un `IDENT` que no nombra variante del enum del escrutado liga el escrutado y coincide siempre. Si ningún brazo coincide, el valor es `0` (`Int`). Los literales y regex en el patrón no mueven el `String` del escrutado. Regex inválida: `E_TYPE`. En destino `soso`, un patrón `RSTRING` es `E_TARGET` (no hay `regcomp` en ese runtime).

`load[F32, 4, 4]("w.salt")` es un `postfix`: argumentos de tipo (`F32`, `4`, `4`) y luego la llamada. `4` y `?` son `type_arg`, no un tipo con el número por nombre.

`elsif` va al mismo nivel que el `if` que acaba de cerrar su suite, y la lista crece por la izquierda. Sin `else`, el `if` vale `Unit` aunque haya `elsif`; las suites se ejecutan pero no son el valor. Con `else`, el valor es el de la rama then. El `else` y cada `elsif` se asocian al `if` interno que acaba de cerrar su suite. `to gpu x + 1` es `(to gpu x) + 1`. `try x + 1` es `try (x + 1)`. `x => x * 2` es `x => (x * 2)`.

### Tipos

```text
type         → ref_type
             | tensor_type
             | named_type
ref_type     → "&" lifetime "mut" type
             | "&" lifetime type
             | "&" "mut" type
             | "&" type
lifetime     → "'" IDENT
tensor_type  → "Tensor" "[" tensor_elem dims "]" "on" place
tensor_elem  → "F32" | "F16" | "BF16" | "I8"
dims         → dims "," dim
             | ε
dim          → INT
             | "?"
named_type   → named_type "[" type_list "]"
             | IDENT "." IDENT
             | IDENT
place        → "cpu" | "gpu" | "tpu" | IDENT
```

`Tensor` seguido de `[` es `tensor_type`. Un elemento fuera de `tensor_elem` (por ejemplo `Float`) es `E_TENSOR_ELEM`.

`&T` es una referencia compartida y `&mut T` una referencia exclusiva. El lifetime `'a` va entre `&` y `mut` (`&'a mut T`) o entre `&` y el tipo (`&'a T`). `'a` es el terminal `'` seguido de un `IDENT`. En una firma de función un lifetime que no se escribe se elide (ver «Memoria»). En un campo de `struct` el lifetime es explícito y tiene que ser un parámetro de tipo del struct; si no, `E_TYPE`. `borrow`, `take` y `keep` no son modos: verlos tras un tipo es `E_PARSE`.

### Cadenas

`STRING` es una cadena entre comillas dobles con escapes `\` habituales. No interpola: `{` y `}` son texto.

`DSTRING` es el identificador `d` inmediatamente seguido de `STRING`, sin espacio entre ambos. Es azúcar de `strdup` aplicado a esa cadena: el árbol es una llamada a `Ident("strdup")` con un argumento `STRING`, no un literal. No interpola. Un `d` suelto o seguido de espacio sigue siendo `IDENT`.

`FSTRING` es el identificador `f` inmediatamente seguido de `STRING`, sin espacio entre ambos. Dentro admite `#{IDENT}`; `##{` es la secuencia literal `#{`. Cualquier otro `#{…}` mal formado es `E_PARSE`. Un `f` suelto o seguido de espacio sigue siendo `IDENT`. Antes de la comprobación de tipos, cada `FSTRING` con interpolación se reescribe a una cadena de `+` que concatena literales y variables (`String`).

`RSTRING` es el identificador `r` inmediatamente seguido de `STRING`, sin espacio entre ambos. Solo es válido como `pattern` en un `match`; en cualquier otra posición es `E_PARSE`. Un `r` suelto o seguido de espacio sigue siendo `IDENT`. El texto del `STRING` es el patrón regex ya descodificado (mismos escapes que `STRING`).

En `add`, si el operando izquierdo es `String`, `+` concatena con efecto `alloc` y el resultado es `String`. El operando derecho también tiene que ser `String`; si no lo es, es `E_TYPE` (con la pista de usar `str_from_int(...)` o `int_to_str(...)`). Si el derecho es `String` y el izquierdo no, es `E_TYPE`. Un asa `Int` que en realidad es un puntero a cadena se tiende con `str_from_int` (identidad sin coste, como `str_as_int` en sentido contrario). La decisión «es concatenación» la toma el tipo inferido del operando izquierdo —literal, variable, parámetro, campo de `struct`, resultado de una función con `-> String` o de un `if`—, no la forma sintáctica del árbol. La cadena asocia a la izquierda.

`+` presta sus dos operandos y da un `String` nuevo: ningún operando cambia (ver «`String`» en «Memoria»). Las hojas de la espina izquierda de una cadena (`((a + b) + c) + d` tiene las hojas `a`, `b`, `c`, `d`) se evalúan de izquierda a derecha, y dos `STRING` seguidos se pliegan en uno al compilar (`x + "a" + "b"` es `x + "ab"`). Bajada: con dos hojas, `str_concat`; con tres o más, una sola `str_concat_n`, que reserva una vez el tamaño total (la suma se comprueba: desbordar es un pánico). Acumulador: en `x = x + …`, cuando el destino es la hoja más a la izquierda y `x` es un `String` con dueño (un local o un parámetro por valor; no una referencia), la asignación **mueve** `x` al `+` y lo reinicializa con el resultado, como `s = s + t` en Rust. Como nadie más puede ver el valor movido, la bajada (`str_append` con una hoja a la derecha, `str_append_n` con varias) puede reutilizar su bloque. Si `x` es una referencia, `x = x + …` es una concatenación normal que liga `x` a un `String` nuevo. Un temporal con dueño de la cadena (un `+` anidado a la derecha, o `int_to_str`, `char_to_str`, `str_slice`, `strdup` o `read_file`) se suelta después de copiarlo («Liberación de locales»); un `str_from_int` o el resultado de una función de usuario no se suelta ahí. `-` en cadenas no está definido; los demás operadores binarios siguen siendo numéricos.

### Derivación de `1 - 2 - 3`

```text
expr ⇒ lambda ⇒ cmp ⇒ add
     ⇒ add "-" mul
     ⇒ add "-" mul "-" mul
     ⇒ mul "-" mul "-" mul
     ⇒ unary "-" unary "-" unary
     ⇒ 1 - 2 - 3
```

El árbol es `Binary(Sub, Binary(Sub, 1, 2), 3)`.

### Atributos

El arranque admite seis atributos, cada uno en un solo sitio. Pueden ir en líneas propias antes del item o del `let`. Cualquier otro nombre, un atributo en otro sitio, un argumento que no sea el indicado o un `@` suelto es `E_PARSE`.

| Atributo | Va delante de | Regla (la violación es `E_TYPE`) |
|----------|---------------|----------------------------------|
| `@frozen` | `let` | El binding no se reasigna |
| `@stack` | `let` | El tipo es de stack y construirlo no asigna |
| `@no_heap` | `fn` | La función no asigna, no recibe ni devuelve un tipo de heap y no tiene `let` de heap |
| `@arena` | `fn`, `method`, `static`, `init` | El retorno es de stack y no es una referencia. Los parámetros son `Copy` o `&T`, salvo un `&mut List` o `&mut Dict`: lo que se guarda ahí se copia al asignador de ese contenedor. Junto con `@no_heap` es `E_TYPE`. Sigue haciendo falta `! alloc` si el cuerpo asigna |
| `@layout(c)` | `struct` | Los campos son solo `Int`, `Float` o `Bool`; `@layout(otro)` es `E_PARSE` |
| `@deprecated("texto")` | `fn`, `method`, `struct`, `enum`, constante | No es error: cada uso del item emite el aviso `W_DEPRECATED` con el texto y la compilación sigue. El argumento es una cadena; sin ella es `E_PARSE` |

Un tipo es de stack si es `Int`, `Float`, `Bool`, `Unit` o un `struct` sin argumentos de tipo cuyos campos lo son. `String`, `List`, `Dict`, `Tensor`, funciones y genéricos son de heap.

`@deprecated` marca un item para retirarlo. Un uso es una llamada al item (función, método ya resuelto a `T__m`, o constructor), un tipo que lo nombra en una firma, en un `let` o en un campo, o el nombre de una constante en una expresión. Se cuentan los usos desde cualquier módulo del grafo, con alias o sin él, y se miran después de cualificar los nombres. El aviso tiene la forma `warning[W_DEPRECATED]: \`nombre\` is deprecated: texto (fichero:línea:columna)` en stderr. Un aviso no cambia el resultado de la compilación ni el código de salida; `W_DEPRECATED` no es un código de error y no aparece en la lista de errores de compilación.

### Reservado

No forman parte del arranque. El parser que las ve emite `E_PARSE` hasta su fase:

```text
parallel_expr → "parallel" suite
```

`@resource`, `@shared`, `@copy`, `@unique` y `@no_gc` usan `attr` pero no están admitidos.

## Tipos

| Tipo | Notas |
|------|--------|
| `Int` | i64 |
| `Float` | f64 (no elemento de tensor) |
| `Bool`, `String`, `Bytes`, `Unit` | |
| `List[T]` | heap, límites comprobados; `T` ∈ Int, Float, Bool, String, struct o enum. `list_push(xs, v)` mueve `v`; `list_set(xs, i, v)` lo pone en la posición `i` (suelta el anterior si es `String`) y devuelve la lista; `list_pop(xs)` quita el último y lo mueve al llamador (pánico si está vacía); `list_get` presta el elemento (`String`, struct describible o enum: el mismo puntero). La copia profunda se inserta solo para una segunda dueña (`return` que vive más que la lista, el parámetro mismo guardado o devuelto, `list_push` / `list_set` / `dict_put`, payload de una variante). Recomponer un struct con campos prestados no copia: el resultado es un alias |
| `Dict[K, V]` | heap; `K` ∈ Int, String; `V` cualquier tipo salvo `Tensor` y funciones (`String`, struct, enum…). `dict_get` devuelve `Option[V]` (el `Option` del preludio: `None` y `Some`, en ese orden) y copia los `String`; `dict_put` presta la clave (con `&` o sin él: no se mueve) y mueve el valor; el `Dict` guarda su propia copia de una clave nueva (un literal se comparte) y la suelta en `dict_remove` o al soltarse; si la clave ya estaba, conserva la suya y suelta el valor anterior si era `String`; `dict_has(d, k)` es `Bool`; `dict_len(d)` es `Int`; `dict_remove(d, k)` borra la entrada (suelta el valor si es `String`) y devuelve `Bool` (`true` si estaba); `dict_keys(d)` y `dict_values(d)` devuelven una `List[K]` / `List[V]` nueva, con las mismas posiciones, en orden no especificado, y copian los `String`. Las claves `Int` (también 0 y negativas) funcionan igual que las `String`. Para recorrer un `Dict` se itera `keys` con `while` y se consulta con `dict_get` |
| `Tensor[Elem, dims…] on lugar` | `Elem` ∈ F32, F16, BF16, I8; `?` eje dinámico |
| structs, enums | |
| `any[P]` | heap; `P` es un protocolo; despacho dinámico solo a través de este tipo |
| genéricos | monomorfización |

### Protocolos

`protocol` e `impl` son contextuales, como `method`: `protocol` solo al inicio de un item; `impl` solo en la cabecera de un `struct` o un `enum`, tras los parámetros de tipo (`struct Point impl Show, Other`). La lista de protocolos crece por la izquierda; el texto canónico deja un espacio tras cada coma. Un `struct` que se llame `any` es `E_PARSE`.

Un `protocol P` declara métodos sin cuerpo: cada línea es `method nombre(self …) -> R ! efectos`, con las mismas reglas de `self` que un `method` de instancia. Un protocolo no tiene parámetros de tipo.

La cláusula `impl P, …` en un `struct` o `enum` sin parámetros de tipo reclama esos protocolos. El compilador comprueba, al cerrar el módulo, que el tipo ya tiene un `method T.nombre` por cada método de cada protocolo, con la misma aridad, tipos (con `self: T`), efectos y modo de `self`. Falta un método: `E_TYPE` en el nombre de ese protocolo en la cabecera. La firma no coincide: `E_TYPE` en el `method`. Un tipo con parámetros de tipo que escribe `impl` es `E_PARSE`. Un nombre que no es protocolo, o repetido en la misma cabecera, es `E_TYPE`.

Solo entra en `any[P]` un valor cuyo tipo nominal reclamó `P`. Pasarlo donde se espera `any[P]` mueve el valor a una caja `{vtable, payload}` y exige `alloc` en la función que coacciona. No hay coerción al revés. `x.m()` con `x: any[P]` llama por la vtable; efectos y `borrow`/`take` salen de la firma del protocolo. Un método que el protocolo no declara es `E_TYPE`. `@no_heap` y `@stack` rechazan `any[P]` como cualquier tipo de heap. Los tipos básicos, `Tensor` y `frame` no admiten `impl` en este corte.

Los tipos nominales no se mezclan: `Int`, `String` y un `struct`/`enum` distinto son incompatibles salvo conversión explícita (p. ej. `.campo` en un newtype de un solo campo). `+` con `String` a la izquierda no es una excepción: exige `String` también a la derecha, y un asa `Int` se convierte explícitamente con `str_from_int` (o se formatea con `int_to_str`).

Un `struct` se construye con `Nombre(arg, …)` en el mismo orden que los campos. Un `struct` de un solo campo es un **newtype transparente**: misma representación que el campo en IR/LLVM. Un `struct` de dos o más campos es un **valor que se mueve**: asignarlo, pasarlo o devolverlo no lo clona. `p.x = v` escribe ese valor: en registros sustituye el campo; si `p` es un elemento de lista (un bloque ya reservado), escribe ese hueco, como `xs[i].x = v`. No hay bloque nuevo ni copia de los hijos. El compilador lo reparte en un registro por campo (los structs anidados, aplanados), sin reservar memoria: un parámetro struct son sus campos como parámetros, y un resultado struct se escribe en un búfer de pila del llamador. Solo se encajona en el heap (un bloque de `i64`, uno por campo) cuando entra en algo que guarda `i64` sin tipo: contenedores (`List`, `Dict`), el payload de un enum y las funciones del runtime. Leer un campo sigue prestando. Recomponer un struct con campos sacados de una referencia (`P(p.name, p.n)` con `p: &P`) es `E_BORROW` si un campo no es `Copy`; `p.name.clone()` lo hace válido. Copiar un struct es `x.clone()` (efecto `alloc`); no hay copia implícita. Pasar un struct por valor lo mueve: si el llamador lo usa después es `E_MOVED`. Para leerlo sin moverlo se pasa `&P`. Quedan fuera del reparto los structs genéricos y los que tienen un campo `Tensor` (los `frame`). Un enum se construye con el nombre de variante (`None`, `Some(x)`, …); en ejecución vale un `i64`: una variante sin campos es su tag; una variante con campos (uno o varios) es la dirección de una caja del heap `[tag, campo0, campo1, …]`. El payload va siempre en caja, sea cual sea su tipo. La caja de un local que solo se lee con `match` se libera al salir del ámbito; los campos con dueño de la caja todavía no (pendiente del glue de drop por tipo).

### Métodos

`method Tipo.nombre(params) -> R ! efectos` define una función `Tipo__nombre` cuyo primer parámetro es `self: Tipo`, implícito. `method` es contextual: solo es palabra clave al inicio de un item (`method` sigue valiendo como nombre de parámetro o variable). `self` no es palabra clave: es el identificador del primer parámetro, y un `self` en otro sitio es un nombre normal. El receptor se escribe `self` (por valor: mueve), `&self` o `&mut self` (`self` sin tipo; `self: T` es `E_PARSE`). Los atributos de `fn` valen delante de `method`, `static` e `init` (`@no_heap`, `@arena`).

`expr.nombre(args)` es una llamada de método cuando `expr` no es un calificador de módulo. El compilador la resuelve por el tipo inferido de `expr` (un tipo nominal `T`) y la baja a `T__nombre(expr, args)`; no hay dispatch dinámico. `self` es un valor como cualquier `struct`: `self.x = v` escribe ese valor, y un método que «muta» devuelve el valor nuevo.

El receptor es un `struct`, `enum` o `frame` definido en el mismo módulo y sin parámetros de tipo; el método no los tiene tampoco (`E_PARSE`). Los métodos de un tipo importado se llaman igual (`p.m()`), pero no se pueden definir fuera de su módulo. Son `E_TYPE`: un receptor que no es de este módulo, builtin (`String`, `List`, …) o genérico; un método repetido o que choca con una `fn Tipo__nombre`; `__` en el nombre del tipo o del método; `expr.nombre(…)` sin método `nombre` en el tipo de `expr`; la aridad y los tipos de los argumentos (reglas de `fn`, con `self` como primer argumento); y que un parámetro o `let` con el mismo nombre que un alias de `import … as` sirva de base de `x.f(…)`. Una forma mal construida (`method` sin `Tipo.nombre`) es `E_PARSE`. El nombre `Tipo__nombre` sigue el esquema `modid__nombre` de «Módulos»: si `Tipo` choca entre módulos, el método se llama `modid__Tipo__nombre`.

`static Tipo.nombre(params) -> R` define `Tipo__nombre` sin `self`, y se llama `Tipo.nombre(args)`. `init Tipo(params)` define el constructor `Tipo__new`: el resultado es siempre un `Tipo` (no se escribe `->`; el cuerpo evalúa a un `Tipo`, normalmente `Tipo(…)`), admite efectos, y se llama `Tipo.new(args)`. Solo hay un `init` por tipo, y `static Tipo.new` choca con él (`E_TYPE`). `static` e `init` son contextuales, como `method`. En `T.nombre(args)`, `T` es un nombre de `struct`, `enum` o `frame` que ningún local sombrea; si hay un local con ese nombre, es una llamada de método sobre ese valor. Llamar un estático sobre un valor (`p.make()`), un método de instancia sobre el tipo (`P.get()`), o un estático inexistente es `E_TYPE`. Valen las mismas reglas de receptor, `__` y aridad que en `method`; los parámetros de tipo no se admiten en ninguno (`E_PARSE`).

Los tipos básicos tienen métodos que son el builtin con el receptor como primer argumento: `xs.push(v)` es `list_push(xs, v)`, con sus mismas reglas de tipos, efectos y ownership (`xs.push(s)` mueve `s`). Se resuelven por el tipo inferido del receptor y no se pueden redefinir ni ampliar con `method`.

| Tipo | Métodos (builtin) |
|------|-------------------|
| `String` | `len` (`str_len`), `eq`, `contains`, `slice`, `char`, `hash`, `concat`, `skip`, `dup` (`strdup`), `to_int` (`str_as_int`), `index_of` (`str_index_of`), `index_from` (`str_index_from`) (`str_*`) |
| `List[T]` | `len`, `push`, `get`, `set`, `pop` (`list_*`) |
| `Dict[K, V]` | `get`, `put`, `has`, `len`, `remove`, `keys`, `values` (`dict_*`) |
| `Bytes` | `len`, `get_u8`, `get_u16`, `get_u32`, `get_i64`, `set_u8`, `set_u16`, `set_u32`, `set_i64`, `fill`, `copy`, `ptr` (`bytes_ptr`), `to_str` (`bytes_to_str`), `write_str` (`str_bytes_copy`) (`bytes_*`) |
| `Int` | `to_str` (`int_to_str`), `to_float` (`int_to_float`) |
| `Float` | `to_int` (`float_to_int`), `to_str` (`f64_bits_to_str`), y las funciones matemáticas `sqrt`, `abs`, `floor`, `ceil`, `sin`, `cos`, `exp`, `log`, `pow` (`float_*`) |

Los constructores de los tipos básicos se llaman sobre el tipo y llevan los argumentos de tipo tras el nombre, como `list_new[Int]()`:

| Llamada | Builtin |
|---------|---------|
| `List.new[T]()` | `list_new[T]()` |
| `Dict.new[K, V]()` | `dict_new[K, V]()` |
| `Bytes.new(n)` | `bytes_new(n)` |
| `Bytes.view_mem(ptr, len)` | `bytes_view_mem(ptr, len)` |
| `String.from_char(c)` | `char_to_str(c)` |

Faltar argumentos de tipo (`List.new()`), sobrar o faltar alguno (`Dict.new[Int]()`) o llamar un estático que no existe (`List.nope[Int]()`) es `E_TYPE`. A diferencia de `list_new()`, que sin argumento de tipo es `List[Int]`, aquí el tipo del elemento siempre es explícito.

Las funciones matemáticas de `Float` (`float_sqrt`, `float_abs`, `float_floor`, `float_ceil`, `float_sin`, `float_cos`, `float_exp`, `float_log`, `float_pow`) toman y devuelven `Float` (`Int` como argumento es `E_TYPE`) y usan libm, que el destino `soso` no tiene: ahí son `E_TARGET`.

Un método que no está en la tabla es `E_TYPE` (`type String has no method nope`). Las funciones `str_len`, `list_push`… siguen existiendo y generan el mismo IR.

Ningún método de `String` escribe en sus bytes («`String`» en «Memoria»). `concat` es `+` de dos hojas: presta los dos y da uno nuevo. `slice` copia el trozo: el resultado no presta de `s`, no lleva lifetime y sobrevive a `s`. `dup` y `clone` copian entero. No hay `append`: `s.append(t)` es `E_TYPE` (`type String has no method append`) y `str_append(s, t)` también (`unknown function`); se escribe `s = s + t`.

Lugar: `cpu`, `gpu`, `tpu` o parámetro `p`.

## Constantes

Un item `NOMBRE = expr`, con el tipo opcional (`NOMBRE: Tipo = expr`), es una constante del módulo. El nombre cumple `^[A-Z][A-Z0-9_]*$`: una mayúscula ASCII y, detrás, mayúsculas, dígitos o `_`. `max = 1` o `Max = 1` es `E_PARSE`. No es una palabra clave. La mayúscula solo marca este item: un `let` y una `fn` con el mismo aspecto de nombre siguen siendo un local y una función. Una constante y otro item del módulo no pueden llamarse igual (`E_TYPE`).

El tipo se infiere de la expresión, como en un `let`. Si se anota y no coincide, `E_TYPE`. La expresión es constante cuando es un literal `INT`, `FLOAT`, `true`, `false` o `STRING` (no `d"…"`, `f"…"` ni `r"…"`), el nombre de otra constante visible, un paréntesis, `-` sobre `Int` o `Float`, `!` sobre `Bool`, `+ - * / %` sobre `Int` (el mismo wrap que en ejecución; `/` y `%` entre 0 son `E_TYPE`), `&&` y `||` sobre `Bool` o `Int` (resultado `Bool`, con el mismo cortocircuito), `== != < <= > >=` sobre `Int` (resultado `Int`, `0` o `1`, como fuera de una constante) o `+` de dos `String` (un solo literal). Una llamada, un `if`, un `match`, un tensor, una lista, un struct o aritmética de `Float` es `E_TYPE`. Un ciclo (`A = A`, o `A` → `B` → `A`) es `E_TYPE`. Se puede nombrar una constante de más adelante en el mismo módulo, o una importada.

Quien importa recibe el valor ya calculado, junto con el tipo: no se copia el item. Sin `as` el nombre se aplana; con `as` o con el calificador implícito se usa `alias.NOMBRE`. Un uso es un rvalue. Asignar (`MAX = 1`) es `E_TYPE`. Un `let` o un parámetro con el mismo nombre sombrea la constante en ese ámbito. Un `String` constante es un literal de rodata (`cap` 0): se puede usar varias veces y no se mueve ni se libera. `Int`, `Bool` y `Float` son Copy. `Bool` baja como el entero `0` o `1`. Otro atributo que no sea `@deprecated("…")` es `E_PARSE`.

## Módulos

Un fichero es un módulo. `import path` enlaza las firmas de los módulos alcanzables. No se copia el cuerpo al AST del raíz. El preludio no se inyecta; hace falta `import "std/prelude.sal"` (o la ruta resuelta equivalente) para `Option`, `Result` y las primitivas documentadas allí.

`import path as alias` importa ese módulo bajo un calificador: sus `fn`, `struct`, `enum` y constantes solo son accesibles como `alias.nombre` (llamadas, tipos, patrones de variante y constantes). No se aplanan en el entorno del que importa.

Sin `as`, el importado se aplana en transitivo como antes: `fn`, `struct`, `enum` y constantes del importado y de sus imports entran en el entorno del que importa. Además, el último segmento del path (sin `.sal`) es un calificador implícito sobre los items definidos en ese fichero (`import lexer` → `lexer.tokenize`; `import "std/prelude.sal"` → `prelude.Option`). El calificador no cubre items reexportados por transitividad. Un local o parámetro con el mismo nombre sombrea el calificador implícito.

La ruta se resuelve, en orden: desde el directorio del fichero que importa; desde la raíz del proyecto; desde `sal_modules/` en esa raíz; desde los `path` de `[dependencies]` en `Sal.toml`; desde los paquetes bloqueados en `Sal.lock` (directorio en la caché global de paquetes). Un path `nombre/resto` enlaza `sal_modules/nombre/resto`, o un paquete cuya clave en `Sal.toml` o cuyo `name` en `Sal.lock` coincide con `nombre`. El texto del import no cambia si el compilador está instalado en otro sitio: los módulos del proyecto están en `sal_modules/`. `sal install [--update]` hace `fetch` y enlaza (symlink) cada dependencia de `Sal.toml` en `sal_modules/<nombre>`: `path` → ese directorio, `git` + `version` → la caché del lock, `{ std = "io" }` → `std/io` del compilador (`SAL_STD` lo sustituye). Un `sal_modules/<nombre>` que no sea symlink se rechaza. Acepta `IDENT` con `.`, o `STRING`; si falta `.sal`, se prueba añadiéndolo. Tras el path, `as` es contextual (el `IDENT` `as` justo después del path).

`Sal.toml` admite dependencias locales `path = "…"` y dependencias git `git = "…"` con `version = "…"` (requisito semver, semántica caret). `sal fetch` resuelve tags `vX.Y.Z` en el remoto, materializa cada paquete en la caché (`$SAL_PACKAGES` o `~/.sal/packages/<nombre>-<versión>/`) y escribe `Sal.lock` con `name`, `version`, `git` y `rev`. También admite releases de GitHub: `libx = { github = "owner/repo", version = "0.1" }`. `sal fetch`/`sal install` consultan `GET /repos/owner/repo/releases` (con `curl`; `$GITHUB_TOKEN` para repos privados; `$SAL_GITHUB_API` cambia la raíz de la API), eligen la release publicada (sin borradores ni prereleases) de mayor versión semver cuyo tag `vX.Y.Z` cumple el requisito, descargan el tarball del tag y lo extraen en la misma caché. El lock guarda `git = "github:owner/repo"`, `rev` (el tag) y `sha256` del tarball, que se verifica si hay que volver a descargarlo. `sal build` y `sal check` no acceden a la red; si falta un paquete del lock en la caché, la resolución falla con `E_TYPE` («import not found»).

Si el mismo nombre de item choca entre módulos del grafo, el compilador asigna un símbolo de enlace único `modid__nombre` (con `modid` derivado de la ruta del módulo) solo a los items en conflicto; el resto conserva su nombre. Las referencias calificadas se resuelven a ese símbolo.

Ruta inexistente, choque de nombre entre importados aplanados o con items locales, dos `as` con el mismo alias, un alias que coincide con un item visible, referencia `alias.x` inexistente, calificador implícito ambiguo en uso, o ciclo en el grafo de imports: `E_TYPE` en el span del `import` o de la referencia.

`sal build` del módulo raíz compila cada módulo alcanzado a su objeto y enlaza esos objetos con el runtime.

## Memoria

El modelo de memoria de sal es el de Rust, con `@arena` como única extensión. Cada valor con dueño tiene un solo dueño y se mueve por defecto; se presta con `&T` y `&mut T` (con lifetimes); se copia solo con `clone()` explícito; y se libera al salir de su ámbito con la glue de su tipo. No hay modos de parámetro inferidos (`borrow`/`take`), ni copias implícitas, ni heurísticas de «valor fresco». Si esta sección y la implementación difieren, manda esta sección y la divergencia se cierra en el mismo cambio.

- Stack por defecto; heap para `String`, `List`, `Dict`, `Bytes`, cajas de enum y de struct, y tensores. Un `struct` de varios campos va en registros salvo que se preste: `&p` materializa el valor.
- Move por defecto. `Copy`: `Int`, `Float`, `Bool`, `Unit` y un struct cuyos campos lo son. `&T` compartida es `Copy`. `&mut T` no lo es.
- `&T` presta y `&mut T` presta en exclusivo. Pueden vivir en un local, en un campo (`struct V['a]`) y en un retorno. No hay un modo `borrow`/`take`: el tipo lo dice. `borrow`, `take` y `keep` tras un tipo son `E_PARSE`.
- Elisión de lifetimes en una firma, con las tres reglas: cada lifetime elidido de entrada es distinto; si hay exactamente uno, el retorno elidido usa ese; si el receptor es `&self` o `&mut self`, el retorno elidido usa el de `self`. Si el retorno elide un lifetime y ninguna regla aplica, `E_BORROW`. En un campo el lifetime es explícito.
- Un valor con dueño (`String`, `List`, `Dict`, `Bytes`, enum, tensor, struct con algún campo que no sea Copy) se mueve al ligarlo a otro nombre, devolverlo, enviarlo con `to`, pasarlo donde se espera ese tipo por valor, guardarlo con `push`/`set`/`put` o usarlo como argumento de un constructor. Usarlo después es `E_MOVED`. Pasar `&x` no lo mueve. Leer un campo o llamar un método pela referencias. El receptor de un método se autoreferencia (`p.m()` con `&self`); un argumento de función, no. El constructor de un `frame` no mueve sus columnas.
- Leer por valor un campo que no es Copy (`n = p.name`) mueve ese campo: `p` entero pasa a `E_MOVED`, los demás campos siguen usables, y `p.name = v` lo repone (solo con `p` local como base directa). Un lugar que se mueve solo en algunas ramas se suelta detrás de una bandera `__live`. Mover en un `while` un valor que sigue vivo en el arco de vuelta es `E_MOVED`.
- Un préstamo dura hasta el último uso de la referencia que lo guarda. `&mut` no se solapa con ningún otro préstamo del mismo lugar; dos `&` sí pueden. Mutar o mover un lugar prestado es `E_BORROW`. Una referencia no sobrevive a su referente: devolver `&` de un local, o guardarla en un sitio que vive más, es `E_BORROW`. `*r` de un `Copy` copia; sacar un no-`Copy` de una referencia es `E_BORROW`. La copia profunda es `x.clone()` (efecto `alloc`); sin ella no hay copia. `Tensor.clone` es `E_TYPE`.
- `&mut T` vale donde se espera `&T` (se represta compartida); `&T` donde se espera `&mut T` es `E_TYPE`. `&r` con `r` una referencia represta su referente. Mutar a través de una `&T` (asignar un campo, `push`/`set`/`put`, un método `&mut self`) es `E_BORROW`.
- `match` sobre un valor con dueño mueve el valor cuando un brazo liga un payload que no es `Copy`: usarlo después es `E_MOVED`. Un brazo que no liga nada, o que solo liga payloads `Copy`, no lo mueve. `match` sobre una referencia (`match &x`, un parámetro `&T`, un campo tras una referencia) liga los payloads como referencias: sacar de ahí un no-`Copy` por valor es `E_BORROW` y se escribe `.clone()`.
- `x.clone()` toma `&self`: no mueve `x`. El clon no comparte nada con el original: un struct copia cada campo (sus `String`, `List`, `Dict`, `Bytes`, enums y structs anidados), y una lista o un dict copian cada elemento.
- `xs.get(i)` es `&T`, `xs.get_mut(i)` es `&mut T`, `xs.pop()` mueve. Si `T` es `Copy`, `xs.get(i)` da el valor (la copia de `*r`). `push`, `set` y `put` toman `&mut` del contenedor, mueven el valor y devuelven `Unit`. Un `Dict` posee sus claves `String`. Un contenedor posee siempre sus elementos.
- En `p.m(…)`, `p` es el primer argumento: `&self`, `&mut self` o `self`.
- Atributos admitidos: `@frozen`, `@stack`, `@no_heap`, `@arena`, `@layout(c)` (ver Atributos). `@resource` y el resto, reservados.
- Un `struct` se mueve. Asignar un campo escribe ese valor: si es un elemento de lista, la lista ve el cambio. Los hijos siguen siendo de quien posee el struct, salvo que ese campo se mueva.

### Funciones `@arena`

Durante la llamada, el heap del host (`String`, `List`, `Dict`, `Bytes`, cajas de enum y de struct) sale de páginas de 64 KiB de una región por hilo. Una `@arena` anidada o recursiva apila otra región; un llamado sin `@arena` usa la región que haya activa. Al salir se ejecuta la liberación de los locales y después se descartan las páginas: un `free` de un puntero interior es un no-op. El retorno es de stack (`Int`, `Float`, `Bool`, `Unit` o un struct cuyos campos lo son) y no es una referencia. Devolver heap o una referencia es `E_TYPE`.

Un parámetro es `Copy` o `&T`. La única escritura permitida sobre un parámetro es un `&mut List` o `&mut Dict`: lo que se guarda ahí (`put`, `push`, `set`) se copia en profundidad al asignador que ya posee ese contenedor (la región en cuya página cae su puntero, o el `malloc` del sistema). La copia de la región se abandona y muere con las páginas. El crecimiento del contenedor reserva en ese mismo asignador. `get` presta. La copia cubre `String`, `List`, `Dict`, `Bytes`, enum y struct describible; un `Tensor` es `E_TYPE`. gpu y tpu no entran en las páginas: su `free` sigue siendo el de su lugar. Un pánico puede no ejecutar el descarte: la fuga de esas páginas es aceptable; un uso tras liberar no. `sal_region_begin` / `sal_region_end` (la región fusionada de tensores) es otro mecanismo y no cambia.

### Liberación de locales

El runtime usa `malloc`/`free`: lo que se libera vuelve al sistema. Cada tipo con dueño tiene una función de liberación (`drop.<Tipo>`): `String` llama a `sal_free`; `Bytes` a `sal_bytes_free` (una vista solo suelta la cabecera); un tensor a `sal_tensor_free` o, en una región fusionada, a `sal_place_free`; `List[T]` y `Dict[K, V]` recorren sus elementos con la liberación de `T` (y las claves `String`) y luego sueltan el bloque; un struct suelta sus campos en orden de declaración y, si tiene dos o más campos (bloque en el heap), suelta también ese bloque (`sal_struct_free_box`); un enum hace `switch` por tag; `&T`, `&mut T` y un `Copy` no sueltan nada. Un struct de un solo campo es transparente: el valor es el campo, sin bloque propio.

Una lista de structs cuyo descriptor es `S(..)` guarda los campos de cada elemento en su propio bloque, con el paso del descriptor: no hay un bloque por elemento. `xs.get(i)` presta un puntero interior a esos campos; `push` y `set` copian los campos del valor y liberan la caja temporal que construyó el compilador; `pop` devuelve una caja nueva con los campos, que el llamante posee; `xs.clone()` copia cada elemento con `clone.<Struct>` a través de una caja temporal. Soltar la lista libera los campos en el sitio con el descriptor, sin liberar un bloque por elemento. Un struct sin descriptor (tiene `List`, `Dict`, `Bytes` o parámetros de tipo) sigue guardado en un bloque por elemento: la lista guarda `drop.elem.<Struct>`, que llama a `drop.<Struct>` (campos y bloque). Una lista de listas guarda `sal_list_free_elem`.

Cada struct con glue y sin descriptor de elemento tiene además `clone.<Struct>`: copia cada campo con la copia de su tipo y devuelve un valor nuevo. `x.clone()` de ese struct la llama, y la lista de esos structs la guarda junto a su glue (`sal_list_set_clone`) para que `xs.clone()` copie cada elemento. El clon de un struct describible es `sal_desc_copy` con su descriptor.

El compilador inserta esa llamada al salir del ámbito, antes de cada `return` (el valor se calcula antes) y al reasignar un local, en orden inverso de declaración, para cada valor con dueño que en ese camino está inicializado y no se ha movido. Si está movido solo en algunas ramas, la llamada va detrás de la bandera `__live`. Al reasignar, el valor nuevo se calcula primero (puede leer el viejo), después se suelta el viejo y el nombre toma el nuevo. Un contenedor que una función devuelve por valor es del llamante, que lo suelta como cualquier otro local; lo mismo el resultado de `x.clone()`. Un temporal con dueño se suelta al final de la sentencia cuando el destino no se lo quedó. Un `free` escrito a mano consume el valor: no se suelta otra vez.

Los `String` que viajan como asas `Int` (`str_as_int`, `vec_push`, `map_put`) quedan fuera de este modelo: no se sueltan. Un programa que termina sin pánico no fuga; un pánico puede fugar.

#### Frontera de asas

Estas operaciones trabajan con direcciones y quedan fuera de las garantías de propiedad, préstamo e inmutabilidad de esta sección. El compilador no comprueba que su uso sea correcto, y no se usan para alterar un valor que tiene dueño:

| Operación | Qué deja fuera del modelo |
|-----------|----------------------------|
| `str_as_int(s)`, `s.to_int()` | La dirección de los bytes de `s` como `Int`. `s` escapa y no se suelta |
| `str_from_int(h)` | Un `String` que es esa dirección, sin copia y sin dueño propio |
| `str_share(s)`, `list_push_share` | Un segundo nombre para el mismo bloque |
| `select_str(c, a, b)` | Devuelve uno y suelta el otro, aunque alguien lo siga nombrando |
| `bytes_ptr(b)`, `bytes_view_mem(p, n)` | La dirección de un búfer como `Int`, y una vista escribible sobre cualquier dirección |
| `vec_*`, `map_*` | Contenedores de asas `Int` sin glue |
| `syscall` | Llamada al sistema con argumentos `Int` |

Un programa que solo usa el resto del lenguaje no puede crear dos dueños de un bloque, ni escribir en los bytes de un `String`, ni usar un valor ya liberado.

### `String`

Un `String` es inmutable: una vez construido, su contenido no cambia. Reasignar la variable está permitido (liga el nombre a otro valor y suelta el viejo); ninguna operación fuera de la «Frontera de asas» escribe en los bytes de un `String` que alguien puede leer. Un `String` se construye entero en un bloque que todavía no tiene dueño (`+`, `strdup`, `str_slice`, `int_to_str`, `bytes_to_str`, `read_file`…) y solo después se entrega. Un préstamo `&String` ve siempre el mismo contenido mientras vive.

Bloque alineado a 16: `[cap: i64][len: i64][bytes…][0]`. El valor `String` es un puntero a `bytes` (terminado en NUL, como `char*` en C).

| `cap` | Significado |
|-------|-------------|
| `0` | Literal estático en rodata: nunca se escribe ni se libera; se comparte entre hilos y entre dueños. |
| `> 0` | Heap: un solo dueño (`move`); la base del bloque es `bytes - 16`. El bloque solo se reutiliza para el resultado de un acumulador `x = x + …` («Expresiones»), que mueve `x`: sin otro dueño ni préstamo vivo, el valor viejo ya no lo ve nadie. |

`len` es `*(bytes - 8)` (longitud en bytes, sin contar el NUL). La cadena vacía es un único estático compartido. Una longitud o una capacidad que no cabe en `i64` es un pánico, no un bloque más corto.

`String` vive en `cpu`. `to gpu` / `to tpu` sobre un `String`, o un `String` usado dentro de `on gpu` / `on tpu`, es `E_PLACE`. Para usar bytes en dispositivo: `str_bytes(s) -> Tensor[I8, ?] on cpu` y después `to gpu` (única transferencia).

### `Bytes`

Bloque `{ data: ptr, len: i64 }` en heap. El valor `Bytes` es un puntero al bloque (asa `Int` en IR). Primitivas:

- `bytes_new(n) -> Bytes ! alloc`
- `bytes_len`, `bytes_ptr`
- `bytes_get_u8/u16/u32/i64`, `bytes_set_u8/u16/u32/i64` — solo `Int`; desbordamiento en LLVM llama `sal_panic`
- `bytes_fill`, `bytes_fill_u32`, `bytes_copy`, `str_bytes_copy`
- `bytes_to_str -> String ! alloc`, `bytes_view -> Bytes ! alloc`, `bytes_view_mem -> Bytes ! alloc`

Una vista (`bytes_view`, `bytes_view_mem`) es una cabecera en heap cuyo `data` apunta fuera; liberar la vista no libera ese buffer (solo la cabecera).

`Bytes` es mutable, pero nunca comparte bloque con un `String`: `bytes_to_str` (`b.to_str()`) copia los bytes a un `String` nuevo, y `str_bytes_copy` (`b.write_str(off, s)`) copia los de `s` al búfer de `b`. Escribir después en `b` no cambia ningún `String`. La única forma de apuntar un `Bytes` a los bytes de un `String` es `bytes_view_mem` sobre `str_as_int`, que está en la «Frontera de asas».

`Bytes` vive en `cpu` (misma regla de lugar que `String`).

### Enteros

Precedencia (de menor a mayor): `||`, `&&`, comparación, `|`, `^`, `&`, desplazamiento (`<<` `>>`), suma, producto (incluye `%`). `%` es resto con signo del dividendo (`srem`). `<<` y `>>` son desplazamiento lógico/aritmético sobre `Int` (`shl`/`ashr`); operadores bitwise solo `Int` (`E_TYPE` con `Float`).

### `syscall`

`syscall(n, a0, a1, a2, a3, a4, a5) -> Int ! io` — siete argumentos `Int`. Disponible al compilar para **soso** (kernel) o **`--target linux`** (shim en `runtime/soso/linux_host_syscall.c`); en otros destinos es `E_TARGET`. En enlace baja a `sal_os_syscall`.

Números estables (ABI soso, ver `std/io/abi.sal`): entre otros, `PIPE` 21, `SPAWN_IO` 22, `CHDIR` 23, `GETCWD` 24, `STAT` 6, `WAIT` 11, `CLOCK_GETTIME` 72, `GETRANDOM` 78, `TCP_CONNECT` 29, `DNS_RESOLVE` 40. `PIPE` devuelve `(escritura << 32) | lectura`. `WAIT` devuelve `(pid << 8) | (código & 0xff)`. En `SpawnIo`, stdio usa `FD_INHERIT_TTY` (-1) o un fd del padre; `FD_CLOSED` (-12) deja el slot vacío.

## Dispositivos

- `to gpu expr`, `to cpu expr`, `to tpu expr` — única transferencia (`E_PLACE` si se mezcla sin `to`).
- `on p` … bloque en dispositivo; `on gpu kernel` rechazado en tpu (`E_DEVICE`).
- `on gpu kernel i, j in Tensor[…] on gpu` (o `in` un binding tensor): `i`, `j` son `Int` sobre la forma estática; el compilador reparte el grid. Sin `kernel_spec`, el kernel escalar sigue siendo válido en gpu.
- `@shared` y barreras de bloque siguen reservados (`E_PARSE`); la memoria compartida de `matmul` en gpu es interna del runtime.

## Modelos

Operaciones: `matmul`, `qmatmul`, `attention`, `map`, `reduce`, `softmax`, `reshape`, `transpose`, `relu`, `silu`, `rms_norm`, `rope`, `tensor_append`, `argmax`, `load`, `str_bytes`, `where`, `tensor[…]`.

`reshape(t, d0, …)` y `transpose(t)` cambian la forma en el tipo (vista si el layout lo permite). `softmax(t, eje)` y `reduce(t, eje)` actúan sobre el eje indicado (recorrido en orden de índice). `attention(q, k, v, causal)` con `q` `[?, H, D]`, `k`/`v` `[?, Hk, D]`, `H` múltiplo de `Hk`; `causal` es `Bool`. `qmatmul(a, w, escala, grupo)`: activación `F16`/`BF16`/`F32`, peso `I8`, escala en el elemento de `a`. `tensor_append(cache, fila)` crece el eje `?`. `argmax(t, eje) -> Int`.

### Operadores columnares

Si un operando de `+`, `-`, `*`, `/`, `==`, `!=`, `<`, `<=`, `>`, `>=` o el prefijo `-` es un `Tensor`, la operación es elemento a elemento. `String + String` sigue siendo concatenación.

- Mismo lugar en los dos tensores; un escalar `Int` o `Float` se difunde. Dos lugares distintos: `E_PLACE`.
- Mismo elemento; el escalar se estrecha al elemento del tensor. Dos elementos distintos: `E_TENSOR_ELEM`.
- Formas compatibles por difusión desde la derecha: ejes alineados al final, con `1` o `?` en un operando; un operando de menor rango se alinea al final; un escalar se difunde. Incompatibles: `E_SHAPE`.
- Aritmética y `-` unario devuelven el mismo elemento y lugar. Comparaciones devuelven `Tensor[I8, misma forma] on lugar` con `0` o `1`. `!` sobre esa máscara invierte `0` y `1` elemento a elemento.

Dentro de `on`, una cadena de estas operaciones del mismo lugar forma una región fusionada (como el epílogo de `map`/`relu` sobre `matmul`): los intermedios únicos no se materializan y entran en `peak_bytes`. Fuera de `on`, cada operador es un lanzamiento en el lugar del tensor.

### Frame

Un `frame` declara columnas con tipos `F32`, `F16`, `BF16`, `I8` o `String`. Tiene un único eje de fila. Una columna numérica es `Tensor[Elem, ?] on p` donde `p` es el parámetro de lugar del frame (o `cpu` si no hay parámetro). Una columna `String` es `List[String]` en `cpu`. Si el esquema incluye `String`, el frame vive en `cpu` y `to gpu` / `to tpu` sobre el frame entero es `E_PLACE`; las columnas numéricas pasan al dispositivo con `to` columna a columna.

Se construye como un struct: `Nombre(col1, …)` en el orden de las columnas. `.col` devuelve la columna. Los operadores columnares actúan sobre cada `Tensor` de columna, no sobre el frame.

`where(tabla, mascara)` filtra filas: la máscara es `Tensor[I8, ?] on p` con la misma longitud de fila. El resultado conserva el esquema y el orden relativo de las filas que pasan.

Fusión en bloque `on`; IR publica `peak_bytes` por lugar.
`load` → cpu, efectos `io`, `alloc`.

## Contenedor `.salt`

Little-endian: magic `SALT`, versión u16, elem u8, rank u8, dims u64[], payload.

`load[...]("p.salt")` de un fichero que no existe o que no es un `.salt` legible es un pánico: el programa escribe `sal panic: cannot load tensor from p.salt` en stderr y sale con 1. Nunca da un tensor nulo.

## Errores de compilación

`E_MOVED`, `E_BORROW`, `E_EFFECT`, `E_PLACE`, `E_SHAPE`, `E_TENSOR_ELEM`, `E_DEVICE`, `E_DEVICE_MISSING`, `E_OOM`, `E_PARSE`, `E_TYPE`, `E_TARGET`.

`E_BORROW`: un préstamo que se solapa con una mutación o con otro préstamo exclusivo, una referencia que vive más que su referente, un lifetime elidido que no tiene origen, o sacar un valor no `Copy` de una referencia. `E_MOVED` sigue siendo usar un valor después de moverlo.

`E_TARGET`: la construcción no existe en el destino de compilación (`--target`), p. ej. `syscall` fuera de `soso` y `linux`. `E_INTERNAL` es un fallo del compilador, no de un programa.

Avisos (no son errores y no paran la compilación): `W_DEPRECATED`, uso de un item marcado con `@deprecated`.

## Instrumentación (`--instrument`)

Eventos: `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN`, `INF`.

## Autohospedaje

IR idéntica compilando el corpus con bootstrap Rust, stage1 (sal←Rust) y stage2 (sal←sal).
