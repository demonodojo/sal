# Especificación del lenguaje sal (arranque)

La gramática de este documento es la definición del lenguaje. Es recursiva por la izquierda en las listas, en los operadores binarios y en los postfijos. Esa recursión es la asociatividad: la derivación izquierda de `1 - 2 - 3` es `(1 - 2) - 3`, la de `a.b.c` es `(a.b).c` y la de `f(x)(y)` es `(f(x))(y)`.

Los prefijos (`try`, `-`, `!`, `to`) y la lambda son recursivos por la derecha. `x => y => z` es `x => (y => z)` y `- - x` es `- (- x)`. Una reescritura recursiva por la derecha de los operadores no es esta gramática: cambiaría el árbol.

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
             | attr_list fn_def
             | attr_list method_def
             | attr_list static_def
             | attr_list init_def
attr_list    → attr_list attr
             | ε
attr         → "@" IDENT ("(" IDENT ")" | ε)

import_def   → "import" path import_as
import_as    → "as" IDENT
             | ε
path         → path "." IDENT
             | IDENT
             | STRING

struct_def   → "struct" IDENT type_params suite_fields
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

enum_def     → "enum" IDENT type_params NEWLINE INDENT variant_list DEDENT
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
method_params → "self" param_mode "," param_list
              | "self" param_mode
              | param_list
static_def   → "static" IDENT "." IDENT "(" param_list ")" "->" type effect_clause suite
init_def     → "init" IDENT "(" param_list ")" effect_clause suite
suite        → NEWLINE INDENT block DEDENT
type_params  → "[" type_param_list "]"
             | ε
type_param_list → type_param_list "," IDENT
                | IDENT
param_list   → param_list "," param
             | param
             | ε
param        → IDENT ":" type param_mode
param_mode   → "borrow"
             | "take"
             | ε
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

Si hay `=`, la izquierda tiene que ser un lvalue: `IDENT`, o un postfijo que solo sea una cadena de campos. Si no lo es, `E_PARSE`. La última línea que es una expresión sin `=` es el valor del bloque en los caminos que no terminan con `return`. Sin esa línea el bloque vale `Unit`; si la función no devuelve `Unit`, el error es `E_TYPE`.

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
type         → tensor_type
             | named_type
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

### Cadenas

`STRING` es una cadena entre comillas dobles con escapes `\` habituales. No interpola: `{` y `}` son texto.

`DSTRING` es el identificador `d` inmediatamente seguido de `STRING`, sin espacio entre ambos. Es azúcar de `strdup` aplicado a esa cadena: el árbol es una llamada a `Ident("strdup")` con un argumento `STRING`, no un literal. No interpola. Un `d` suelto o seguido de espacio sigue siendo `IDENT`.

`FSTRING` es el identificador `f` inmediatamente seguido de `STRING`, sin espacio entre ambos. Dentro admite `#{IDENT}`; `##{` es la secuencia literal `#{`. Cualquier otro `#{…}` mal formado es `E_PARSE`. Un `f` suelto o seguido de espacio sigue siendo `IDENT`. Antes de la comprobación de tipos, cada `FSTRING` con interpolación se reescribe a una cadena de `+` que concatena literales y variables (`String`).

`RSTRING` es el identificador `r` inmediatamente seguido de `STRING`, sin espacio entre ambos. Solo es válido como `pattern` en un `match`; en cualquier otra posición es `E_PARSE`. Un `r` suelto o seguido de espacio sigue siendo `IDENT`. El texto del `STRING` es el patrón regex ya descodificado (mismos escapes que `STRING`).

En `add`, si el operando izquierdo es `String`, `+` concatena con efecto `alloc` y el resultado es `String`. El operando derecho también tiene que ser `String`; si no lo es, es `E_TYPE` (con la pista de usar `str_from_int(...)` o `int_to_str(...)`). Si el derecho es `String` y el izquierdo no, es `E_TYPE`. Un asa `Int` que en realidad es un puntero a cadena se tiende con `str_from_int` (identidad sin coste, como `str_as_int` en sentido contrario). La decisión «es concatenación» la toma el tipo inferido del operando izquierdo —literal, variable, parámetro, campo de `struct`, resultado de una función con `-> String` o de un `if`—, no la forma sintáctica del árbol. La cadena asocia a la izquierda.

Bajada: el **primer** `+` de la cadena es `str_concat` (copia exacta) y los **siguientes** —cuando el hijo izquierdo del AST es otro `+` de cadenas— son `str_append` sobre ese acumulador temporal. Excepción de acumulador: en `x = x + ...`, cuando el destino de la asignación es la hoja más a la izquierda de la cadena, **todos** los pasos bajan a `str_append` sobre `x` (mutación in situ, sin copia); para la propiedad, esa asignación mueve `x` (un parámetro pasa a `take`). En cualquier otro `+` los dos operandos se toman prestados: `+` no consume. Si el operando derecho es un temporal fresco —resultado directo de `int_to_str`, `char_to_str`, `str_slice`, `strdup` o `read_file`— se libera con `sal_free` justo después del `str_concat`/`str_append`; un `str_from_int` o el resultado de una función de usuario nunca se libera ahí. `-` en cadenas no está definido; los demás operadores binarios siguen siendo numéricos.

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

El arranque admite cinco atributos, cada uno en un solo sitio. Pueden ir en líneas propias antes del item o del `let`. Cualquier otro nombre, un atributo en otro sitio, un argumento que no sea el indicado o un `@` suelto es `E_PARSE`.

| Atributo | Va delante de | Regla (la violación es `E_TYPE`) |
|----------|---------------|----------------------------------|
| `@frozen` | `let` | El binding no se reasigna |
| `@stack` | `let` | El tipo es de stack y construirlo no asigna |
| `@no_heap` | `fn` | La función no asigna, no recibe ni devuelve un tipo de heap y no tiene `let` de heap |
| `@layout(c)` | `struct` | Los campos son solo `Int`, `Float` o `Bool`; `@layout(otro)` es `E_PARSE` |
| `@deprecated("texto")` | `fn`, `method`, `struct`, `enum` | No es error: cada uso del item emite el aviso `W_DEPRECATED` con el texto y la compilación sigue. El argumento es una cadena; sin ella es `E_PARSE` |

Un tipo es de stack si es `Int`, `Float`, `Bool`, `Unit` o un `struct` sin argumentos de tipo cuyos campos lo son. `String`, `List`, `Dict`, `Tensor`, funciones y genéricos son de heap.

`@deprecated` marca un item para retirarlo. Un uso es una llamada al item (función, método ya resuelto a `T__m`, o constructor), o un tipo que lo nombra en una firma, en un `let` o en un campo. Se cuentan los usos desde cualquier módulo del grafo, con alias o sin él, y se miran después de cualificar los nombres. El aviso tiene la forma `warning[W_DEPRECATED]: \`nombre\` is deprecated: texto (fichero:línea:columna)` en stderr. Un aviso no cambia el resultado de la compilación ni el código de salida; `W_DEPRECATED` no es un código de error y no aparece en la lista de errores de compilación.

### Reservado

No forman parte del arranque. El parser que las ve emite `E_PARSE` hasta su fase:

```text
parallel_expr → "parallel" suite
```

`@resource`, `@shared`, `@arena`, `@copy`, `@unique` y `@no_gc` usan `attr` pero no están admitidos. `any Protocol` no tiene producción propia: cuando exista, será un `named_type`.

## Tipos

| Tipo | Notas |
|------|--------|
| `Int` | i64 |
| `Float` | f64 (no elemento de tensor) |
| `Bool`, `String`, `Bytes`, `Unit` | |
| `List[T]` | heap, límites comprobados; `T` ∈ Int, Float, Bool, String, struct o enum. `list_push(xs, v)` mueve `v`; `list_set(xs, i, v)` lo pone en la posición `i` (suelta el anterior si es `String`) y devuelve la lista; `list_pop(xs)` quita el último y lo mueve al llamador (pánico si está vacía); `list_get` copia |
| `Dict[K, V]` | heap; `K` ∈ Int, String; `V` cualquier tipo salvo `Tensor` y funciones (`String`, struct, enum…). `dict_get` devuelve `Option[V]` (el `Option` del preludio: `None` y `Some`, en ese orden) y copia los `String`; `dict_put` mueve el valor y suelta el anterior si era `String`; `dict_has(d, k)` es `Bool`; `dict_len(d)` es `Int`; `dict_remove(d, k)` borra la entrada (suelta el valor si es `String`) y devuelve `Bool` (`true` si estaba); `dict_keys(d)` y `dict_values(d)` devuelven una `List[K]` / `List[V]` nueva, con las mismas posiciones, en orden no especificado, y copian los `String`. Las claves `Int` (también 0 y negativas) funcionan igual que las `String`. Para recorrer un `Dict` se itera `keys` con `while` y se consulta con `dict_get` |
| `Tensor[Elem, dims…] on lugar` | `Elem` ∈ F32, F16, BF16, I8; `?` eje dinámico |
| structs, enums | |
| genéricos | monomorfización |

Los tipos nominales no se mezclan: `Int`, `String` y un `struct`/`enum` distinto son incompatibles salvo conversión explícita (p. ej. `.campo` en un newtype de un solo campo). `+` con `String` a la izquierda no es una excepción: exige `String` también a la derecha, y un asa `Int` se convierte explícitamente con `str_from_int` (o se formatea con `int_to_str`).

Un `struct` se construye con `Nombre(arg, …)` en el mismo orden que los campos. Un `struct` de un solo campo es un **newtype transparente**: misma representación que el campo en IR/LLVM. Un `struct` de dos o más campos es un **valor**: se copia al asignarlo, al pasarlo y al devolverlo, y `p.x = v` actualiza solo esa copia. El compilador lo reparte en un registro por campo (los structs anidados, aplanados), sin reservar memoria: un parámetro struct son sus campos como parámetros, y un resultado struct se escribe en un búfer de pila del llamador. Solo se encajona en el heap (un bloque de `i64`, uno por campo) cuando entra en algo que guarda `i64` sin tipo: contenedores (`List`, `Dict`), el payload de un enum y las funciones del runtime. Quedan fuera del reparto los structs genéricos y los que tienen un campo `Tensor` (los `frame`). Un enum se construye con el nombre de variante (`None`, `Some(x)`, …); en ejecución vale un `i64`: una variante sin campos es su tag; una variante con campos (uno o varios) es la dirección de una caja del heap `[tag, campo0, campo1, …]`. El payload va siempre en caja, sea cual sea su tipo. La caja de un local que solo se lee con `match` se libera al salir del ámbito; los campos con dueño de la caja todavía no (pendiente del glue de drop por tipo).

### Métodos

`method Tipo.nombre(params) -> R ! efectos` define una función `Tipo__nombre` cuyo primer parámetro es `self: Tipo`, implícito. `method` es contextual: solo es palabra clave al inicio de un item (`method` sigue valiendo como nombre de parámetro o variable). `self` no es palabra clave: es el identificador del primer parámetro, y un `self` en otro sitio es un nombre normal. El modo de `self` sale del cuerpo, como el de cualquier parámetro, salvo que se escriba: `method T.m(self take, k: Int)` o `self borrow` (`self` sin tipo; `self: T` es `E_PARSE`). Los atributos de `fn` valen delante de `method` (`@no_heap`).

`expr.nombre(args)` es una llamada de método cuando `expr` no es un calificador de módulo. El compilador la resuelve por el tipo inferido de `expr` (un tipo nominal `T`) y la baja a `T__nombre(expr, args)`; no hay dispatch dinámico. `self` es un valor como cualquier `struct`: `self.x = v` solo cambia la copia, y un método que «muta» devuelve el valor nuevo.

El receptor es un `struct`, `enum` o `frame` definido en el mismo módulo y sin parámetros de tipo; el método no los tiene tampoco (`E_PARSE`). Los métodos de un tipo importado se llaman igual (`p.m()`), pero no se pueden definir fuera de su módulo. Son `E_TYPE`: un receptor que no es de este módulo, builtin (`String`, `List`, …) o genérico; un método repetido o que choca con una `fn Tipo__nombre`; `__` en el nombre del tipo o del método; `expr.nombre(…)` sin método `nombre` en el tipo de `expr`; la aridad y los tipos de los argumentos (reglas de `fn`, con `self` como primer argumento); y que un parámetro o `let` con el mismo nombre que un alias de `import … as` sirva de base de `x.f(…)`. Una forma mal construida (`method` sin `Tipo.nombre`) es `E_PARSE`. El nombre `Tipo__nombre` sigue el esquema `modid__nombre` de «Módulos»: si `Tipo` choca entre módulos, el método se llama `modid__Tipo__nombre`.

`static Tipo.nombre(params) -> R` define `Tipo__nombre` sin `self`, y se llama `Tipo.nombre(args)`. `init Tipo(params)` define el constructor `Tipo__new`: el resultado es siempre un `Tipo` (no se escribe `->`; el cuerpo evalúa a un `Tipo`, normalmente `Tipo(…)`), admite efectos, y se llama `Tipo.new(args)`. Solo hay un `init` por tipo, y `static Tipo.new` choca con él (`E_TYPE`). `static` e `init` son contextuales, como `method`. En `T.nombre(args)`, `T` es un nombre de `struct`, `enum` o `frame` que ningún local sombrea; si hay un local con ese nombre, es una llamada de método sobre ese valor. Llamar un estático sobre un valor (`p.make()`), un método de instancia sobre el tipo (`P.get()`), o un estático inexistente es `E_TYPE`. Valen las mismas reglas de receptor, `__` y aridad que en `method`; los parámetros de tipo no se admiten en ninguno (`E_PARSE`).

Los tipos básicos tienen métodos que son el builtin con el receptor como primer argumento: `xs.push(v)` es `list_push(xs, v)`, con sus mismas reglas de tipos, efectos y ownership (`xs.push(s)` mueve `s`). Se resuelven por el tipo inferido del receptor y no se pueden redefinir ni ampliar con `method`.

| Tipo | Métodos (builtin) |
|------|-------------------|
| `String` | `len` (`str_len`), `eq`, `contains`, `slice`, `char`, `hash`, `concat`, `append`, `skip`, `to_int` (`str_as_int`), `index_of` (`str_index_of`), `index_from` (`str_index_from`) (`str_*`) |
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

Lugar: `cpu`, `gpu`, `tpu` o parámetro `p`.

## Módulos

Un fichero es un módulo. `import path` enlaza las firmas de los módulos alcanzables. No se copia el cuerpo al AST del raíz. El preludio no se inyecta; hace falta `import "std/prelude.sal"` (o la ruta resuelta equivalente) para `Option`, `Result` y las primitivas documentadas allí.

`import path as alias` importa ese módulo bajo un calificador: sus `fn`, `struct` y `enum` solo son accesibles como `alias.nombre` (llamadas, tipos y patrones de variante). No se aplanan en el entorno del que importa.

Sin `as`, el importado se aplanan en transitivo como antes: `fn`, `struct` y `enum` del importado y de sus imports entran en el entorno del que importa. Además, el último segmento del path (sin `.sal`) es un calificador implícito sobre los items definidos en ese fichero (`import lexer` → `lexer.tokenize`; `import "std/prelude.sal"` → `prelude.Option`). El calificador no cubre items reexportados por transitividad. Un local o parámetro con el mismo nombre sombrea el calificador implícito.

La ruta se resuelve, en orden: desde el directorio del fichero que importa; desde la raíz del proyecto; desde `sal_modules/` en esa raíz; desde los `path` de `[dependencies]` en `Sal.toml`; desde los paquetes bloqueados en `Sal.lock` (directorio en la caché global de paquetes). Un path `nombre/resto` enlaza `sal_modules/nombre/resto`, o un paquete cuya clave en `Sal.toml` o cuyo `name` en `Sal.lock` coincide con `nombre`. El texto del import no cambia si el compilador está instalado en otro sitio: los módulos del proyecto están en `sal_modules/`. `sal install [--update]` hace `fetch` y enlaza (symlink) cada dependencia de `Sal.toml` en `sal_modules/<nombre>`: `path` → ese directorio, `git` + `version` → la caché del lock, `{ std = "io" }` → `std/io` del compilador (`SAL_STD` lo sustituye). Un `sal_modules/<nombre>` que no sea symlink se rechaza. Acepta `IDENT` con `.`, o `STRING`; si falta `.sal`, se prueba añadiéndolo. Tras el path, `as` es contextual (el `IDENT` `as` justo después del path).

`Sal.toml` admite dependencias locales `path = "…"` y dependencias git `git = "…"` con `version = "…"` (requisito semver, semántica caret). `sal fetch` resuelve tags `vX.Y.Z` en el remoto, materializa cada paquete en la caché (`$SAL_PACKAGES` o `~/.sal/packages/<nombre>-<versión>/`) y escribe `Sal.lock` con `name`, `version`, `git` y `rev`. También admite releases de GitHub: `libx = { github = "owner/repo", version = "0.1" }`. `sal fetch`/`sal install` consultan `GET /repos/owner/repo/releases` (con `curl`; `$GITHUB_TOKEN` para repos privados; `$SAL_GITHUB_API` cambia la raíz de la API), eligen la release publicada (sin borradores ni prereleases) de mayor versión semver cuyo tag `vX.Y.Z` cumple el requisito, descargan el tarball del tag y lo extraen en la misma caché. El lock guarda `git = "github:owner/repo"`, `rev` (el tag) y `sha256` del tarball, que se verifica si hay que volver a descargarlo. `sal build` y `sal check` no acceden a la red; si falta un paquete del lock en la caché, la resolución falla con `E_TYPE` («import not found»).

Si el mismo nombre de item choca entre módulos del grafo, el compilador asigna un símbolo de enlace único `modid__nombre` (con `modid` derivado de la ruta del módulo) solo a los items en conflicto; el resto conserva su nombre. Las referencias calificadas se resuelven a ese símbolo.

Ruta inexistente, choque de nombre entre importados aplanados o con items locales, dos `as` con el mismo alias, un alias que coincide con un item visible, referencia `alias.x` inexistente, calificador implícito ambiguo en uso, o ciclo en el grafo de imports: `E_TYPE` en el span del `import` o de la referencia.

`sal build` del módulo raíz compila cada módulo alcanzado a su objeto y enlaza esos objetos con el runtime.

## Memoria

- Stack por defecto; heap para `String`, `List`, tensores. Un `struct` de varios campos va en registros (ver «Un `struct` se construye…»).
- Move por defecto; copy solo tipos triviales.
- Préstamo solo en argumento de llamada. En `p.m(…)`, `p` es el primer argumento: se presta o se mueve según el modo de `self`.
- Un valor con dueño (`String`, `List`, `Dict`, enum, tensor, struct con algún campo que no sea Copy) se mueve al ligarlo a otro nombre, devolverlo, enviarlo con `to`, pasarlo a un parámetro `take`, guardarlo con `list_push`/`list_set`/`dict_put` (el valor, no el contenedor) o usarlo como argumento de un constructor de struct o de variante. Usarlo después es `E_MOVED`. El modo de cada parámetro sale del cuerpo y es interprocedural: una función que pasa su parámetro a una posición `take` de otra también lo toma. Los tipos que decide el pase salen de la inferencia (el resultado de una llamada, un campo o un `match` ya no se trata como Copy). El valor de cola de una rama de `if` no se mueve por sí mismo. El constructor de un `frame` no mueve sus columnas. Leer por valor un campo que no es Copy (`n = p.name`) mueve ese campo: `p.name` y `p` entero pasan a `E_MOVED`, pero los demás campos siguen usables, y `p.name = v` lo repone (solo con `p` local como base directa). Un local que se mueve solo en algunas ramas se suelta al final detrás de una bandera de drop (`__live_x`) que se baja justo después de la sentencia que lo mueve; si se mueve en un bucle más interno que su declaración, o en una cola de bloque, `match` o `on`, no se suelta (fuga, nunca doble liberación). Un `String` fresco que se pasa directo a un builtin que solo lo lee (`str_len`, `str_eq`, `str_slice`, `print_str`…) es un temporal: se liga a un local anterior a la sentencia y se suelta al final del ámbito. Una `List[S]` local de un struct plano (de 2 a 20 campos `Int`, `Float`, `Bool` o `String`) se suelta, con sus elementos, cuando todo lo que se le guarda es un constructor `S(…)` cuyos `String` son frescos; `list_get` sobre ella devuelve una copia (los `String` se duplican) y `list_set` suelta el elemento viejo.
- Atributos admitidos: `@frozen`, `@stack`, `@no_heap`, `@layout(c)` (ver Atributos). `@resource` y el resto, reservados.
- Un `struct` es un valor: asignar un campo en una copia o dentro de una función llamada no cambia el original.

### Liberación de locales

El runtime usa `malloc`/`free`: lo que se libera vuelve al sistema. El compilador inserta la liberación (`free`, `list_free`, `dict_free`, `enum_free`, `bytes_free`, `tensor_free`, `struct_free` o, cuando una copia struct en caja ya cedió todos sus campos, solo el bloque heap con `struct_free_box`) al salir de ámbito para un local que cumple todo esto:

- se liga una sola vez (`let` o primera asignación, sin anotación de tipo salvo `String`) a un valor fresco. `String`: literal, `int_to_str`, `char_to_str`, `str_slice`, `strdup`, `read_file`, `str_concat`, o una cadena de `+` con algún operando `String` explícito. `List`: `list_new[T]()` con `T` `Int`, `Float`, `Bool` o `String` (una lista de `String` es dueña de sus elementos y `list_free` los suelta). `dict_keys(d)` y `dict_values(d)` también son listas frescas: se sueltan igual (con sus `String`) si no escapan. `Dict`: `dict_new[K, V]()` con `K` `Int` o `String` y `V` `Int`, `Float`, `Bool`, `String`, un enum sin parámetros de tipo o un `struct` de dos o más campos describible (misma regla que el descriptor de listas: suelta valores `String`, cajas de enum y bloques de struct que el dict posee; las claves `String` se prestan). Enum: una variante con campos (`Rect(3, 4)`); la caja guarda en su cabecera qué campos posee (un `String` fresco o la caja fresca de otro enum con campos) y `enum_free` los suelta; un campo que es un nombre o el resultado de una función de usuario no se posee y se deja. `enum_owns(v, i)` devuelve 1 cuando la caja de `v` posee su campo `i` (y `enum_free` lo suelta), de modo que quien libere a mano los demás campos no libere dos veces. `Bytes`: `bytes_new(n)`; `bytes_view` y `bytes_view_mem` reservan solo la cabecera del bloque (vista sobre otro `Bytes` o memoria ajena: `bytes_free` no libera el `data` del padre). `dict_get` devuelve un `Option[V]` fresco: la caja de `Some` se suelta con `enum_free` si no escapa (el `String` dentro lleva bit de dueño cuando `V` es `String`; valores enum o struct se copian en profundidad). Un literal `tensor[[…]]` en un programa escalar es un buffer en el heap del lugar: `tensor_free` si no escapa (en funciones con región fusionada, `to` y la bajada emiten `Drop` sobre el origen y el runtime usa `sal_place_free`).
- no escapa: solo lo leen builtins que prestan (`str_len`, `str_eq`, `str_char`, `str_contains`, `str_skip`, `str_hash`, `str_slice`, `str_concat`, `str_to_f64_bits`, `print_str`, `eprint_str`, `write_file`, `mkdir_p`, `path_readable`, `getenv`, `map_get`, `read_file`, `list_len`, `list_get`, `list_pop`, `dict_has`, `enum_owns`, `bytes_len`, `bytes_get_*`, `bytes_set_*`, `bytes_fill*`, `bytes_copy`, `str_bytes_copy`), el operador `+` y, para un enum, un `match` cuyos patrones son variantes (un patrón que liga el valor entero lo hace escapar). `dict_get` presta el diccionario pero el `Option` devuelto es un valor nuevo: quien lo liga es dueño de la caja salvo que escape. `list_push`, `list_set` y `dict_put` prestan el contenedor solo como sentencia (o ligado a un nombre que empieza por `_`), porque devuelven un alias. Pasarlo a una función de usuario cuyo parámetro lo deja escapar (lo devuelve, lo guarda, lo liga a otro nombre o lo pasa a un parámetro que escapa), a `str_as_int`, a `vec_push`/`map_put`, ligarlo a otro nombre, devolverlo o ponerlo en cola de bloque lo hace escapar y no se libera. Un argumento movido a un constructor de `struct` deja de contarse entre los locales pendientes de liberar en ese ámbito.

El `free` va al final del bloque que declara el local y antes de cada `return` dentro de su ámbito, en orden inverso de declaración. Un valor de cola o de `return` se calcula antes de liberar. Una lista de `String`, un `Dict` con valores `String` o un campo `List[String]`, `List[P]` (con `P` struct describible, misma regla de dueño que en listas) o `Dict[…, String]` dentro de un `struct` solo se libera si todo lo que se guarda en él es un valor fresco (literal, `int_to_str`, `str_slice`, `+`…); guardar un nombre o el resultado de una función de usuario lo deja sin liberar, para no soltar dos veces un valor ajeno. Un `match x` libera la caja de `x` solo si ningún nombre ligado por sus patrones a un campo que pueda tener dueño escapa (un campo `Int`, `Float` o `Bool` no cuenta). Un `match dict_get(…)` descartado como sentencia suelta la caja del resultado en esa sentencia. En `a + b`, un operando izquierdo que es una llamada de cadena fresca (`int_to_str(n) + "x"`) también se suelta tras copiarlo. Un parámetro de usuario es de solo lectura si su función no lo deja escapar, y se calcula como punto fijo sobre todo el programa (empieza optimista, así que la recursión no lo estropea): pasar un local a esa posición no lo hace escapar y lo libera quien lo declara.

Un `struct` local (o un parámetro `take`) libera al final de la función, en la cola de campos, lo que se le movió: campos `String`, cajas enum, `Bytes`, `List[T]` con `T` escalar Copy, `Dict[K, V]` con valores escalares Copy, `List[String]` y `List[P]` describible (cuando la lista es dueña de sus elementos), `Dict` cuyos valores son `String` cuando solo recibieron valores frescos, y de forma recursiva los `struct` anidados. Si un campo `String` o enum sale del struct por valor (`return n.name`, argumento que lo conserva), la cola deja de soltar **ese** campo y sigue con el resto (el nuevo dueño lo libera si no escapa). Si sale un campo contenedor entero (`List`, `Dict` o `Bytes`, p. ej. `return b.xs`), la cola deja de soltar contenedores en ese local. Si se lee un `struct` anidado entero (`q = w.p`), deja de liberar los anidados. Es una fuga posible, nunca una doble liberación. Un local que no cumple la regla no se libera (la memoria no vuelve, pero nunca hay uso tras liberar).

Una lista de enums es dueña de sus cajas solo si todo lo que se le guarda es una llamada a un constructor de variante (`V(…)`) o una variante sin campos (`V`): entonces `list_free` suelta cada caja, `list_set` suelta la anterior y `list_get` devuelve una copia profunda y `list_pop` saca el elemento de la lista; si se liga a un nombre que no escapa (o se usa como selector de un `match`, que el compilador liga antes a un nombre), se suelta al final del ámbito. Lo mismo vale para una lista de `String` dueña. Una lista de enums con cualquier otro valor guardado (un nombre, el resultado de una función) no es dueña: `list_free` solo libera el vector y `list_get` devuelve el mismo valor. Un `list_free` escrito a mano sobre una lista que no es dueña no suelta sus elementos. Un valor fresco que una sentencia descarta (`xs.pop()`, `xs.get(i)`, `int_to_str(n)`) se liga a un temporal y se suelta en esa misma sentencia: el `pop` de una lista dueña y la copia de `get` son frescos. Una función de usuario cuyo retorno es `String` y entrega siempre un valor fresco (cada `return` y la cola son un builtin fresco, un local ligado una vez a uno, un `if`/`match` cuya **cada** rama lo es, o un campo `String` de un parámetro `take`) devuelve un `String` que el llamante posee y se suelta como un builtin fresco. Un `while` en el nivel superior del cuerpo de esa función la excluye de ese conjunto. Una lista de structs (planos o anidados, con campos `String`, enum o struct) es dueña de sus bloques si todo lo que se le guarda es un constructor del struct con campos frescos; `list_get` y `list_pop` devuelven una copia profunda (nuevo bloque heap con los campos) y el struct local que la recibe libera con la cola lo que no se movió antes. Un struct con un solo campo transparente o tipos no describibles en listas sigue sin descriptor de elemento.

Un local ligado a esa copia (`o = list_get(xs, i)` o `list_pop`) se comporta así al pasarlo como argumento: `list_push`, `list_set`, `dict_put` o un constructor de struct absorben la copia entera (el bloque sigue vivo dentro del contenedor o del valor nuevo; no se suelta como local). Una función de usuario cuyo parámetro struct se reparte en campos recibe cada campo por movimiento y deja la caja vacía: el compilador suelta solo el bloque (`struct_free_box`), no los campos ya cedidos. Si el argumento solo se presta (builtin lector, parámetro de solo lectura, operador `+`…) la copia entera sigue siendo del local y, al salir del ámbito sin escapar, entra en la cola (`struct_free`). Si el local puede moverse solo en algunas ramas, la liberación va detrás de la bandera `__live`: `struct_free` cuando aún hay campos vivos y `struct_free_box` cuando una llamada escalarizada vació la caja. El análisis recorre sentencias y la cola del bloque.

Siguen sin liberarse automáticamente: los campos de una caja de enum que no son valores frescos; el `list_pop` de una lista que no es dueña; los `String` que viajan como asas `Int` (`str_as_int`, `vec_push`, `map_put`); dos nombres sobre la misma copia (`z = k`) donde soltar uno sería doble liberación.

### `String`

Bloque alineado a 16: `[cap: i64][len: i64][bytes…][0]`. El valor `String` es un puntero a `bytes` (terminado en NUL, como `char*` en C).

| `cap` | Significado |
|-------|-------------|
| `0` | Literal estático en rodata: inmutable, compartible entre hilos, no se libera; `append` copia. |
| `> 0` | Heap: un solo dueño (`move`); la base del bloque es `bytes - 16`; `append` in situ si cabe. |

`len` es `*(bytes - 8)` (longitud en bytes, sin contar el NUL). La cadena vacía es un único estático compartido.

`String` vive en `cpu`. `to gpu` / `to tpu` sobre un `String`, o un `String` usado dentro de `on gpu` / `on tpu`, es `E_PLACE`. Para usar bytes en dispositivo: `str_bytes(s) -> Tensor[I8, ?] on cpu` y después `to gpu` (única transferencia).

### `Bytes`

Bloque `{ data: ptr, len: i64 }` en heap. El valor `Bytes` es un puntero al bloque (asa `Int` en IR). Primitivas:

- `bytes_new(n) -> Bytes ! alloc`
- `bytes_len`, `bytes_ptr`
- `bytes_get_u8/u16/u32/i64`, `bytes_set_u8/u16/u32/i64` — solo `Int`; desbordamiento en LLVM llama `sal_panic`
- `bytes_fill`, `bytes_fill_u32`, `bytes_copy`, `str_bytes_copy`
- `bytes_to_str -> String ! alloc`, `bytes_view -> Bytes ! alloc`, `bytes_view_mem -> Bytes ! alloc`

Una vista (`bytes_view`, `bytes_view_mem`) es una cabecera en heap cuyo `data` apunta fuera; liberar la vista no libera ese buffer (solo la cabecera).

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

## Errores de compilación

`E_MOVED`, `E_EFFECT`, `E_PLACE`, `E_SHAPE`, `E_TENSOR_ELEM`, `E_DEVICE`, `E_DEVICE_MISSING`, `E_OOM`, `E_PARSE`, `E_TYPE`, `E_TARGET`.

`E_TARGET`: la construcción no existe en el destino de compilación (`--target`), p. ej. `syscall` fuera de `soso` y `linux`. `E_INTERNAL` es un fallo del compilador, no de un programa.

Avisos (no son errores y no paran la compilación): `W_DEPRECATED`, uso de un item marcado con `@deprecated`.

## Instrumentación (`--instrument`)

Eventos: `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN`, `INF`.

## Autohospedaje

IR idéntica compilando el corpus con bootstrap Rust, stage1 (sal←Rust) y stage2 (sal←sal).
