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

Los no terminales van en minúsculas. `ε` es la producción vacía. `IDENT`, `INT`, `FLOAT`, `STRING` y `DSTRING` son terminales del lexer. Lo demás entre comillas es terminal.

### Programa

```text
program      → item_list
item_list    → item_list item
             | ε
item         → import_def
             | struct_def
             | enum_def
             | frame_def
             | fn_def

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
line         → "let" IDENT (":" type | ε) "=" expr
             | "return" (expr | ε)
             | "while" expr suite
             | expr ("=" expr | ε)
```

Si hay `=`, la izquierda tiene que ser un lvalue: `IDENT`, o un postfijo que solo sea una cadena de campos. Si no lo es, `E_PARSE`. La última línea que es una expresión sin `=` es el valor del bloque en los caminos que no terminan con `return`. Sin esa línea el bloque vale `Unit`; si la función no devuelve `Unit`, el error es `E_TYPE`.

`return expr` termina la función con `expr`; el tipo de `expr` tiene que ser el de la función o `E_TYPE`. `return` sin expresión solo si la función devuelve `Unit`. Vale en el cuerpo de la función y, anidado, en `if`, `elsif`, `else`, `while` y `match`. Dentro de `on` o de una lambda es `E_TYPE`. Tras un `return` que siempre se alcanza en el mismo bloque (otra sentencia o la cola) es `E_TYPE`. Un `while` no garantiza el retorno; un `if` sin `else` tampoco. Una rama que retorna no aporta valor al `if`/`match`; el tipo es el de las ramas que sí producen valor. Si todas las ramas retornan, la expresión diverge.

### Expresiones

```text
expr         → "try" expr
             | lambda
lambda       → cmp "=>" expr
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
arm          → pattern "=>" expr
pattern      → "_"
             | INT
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

El parámetro a la izquierda de `=>` en `lambda` es un `IDENT`. `a + b => c` es `E_PARSE`. Dentro de `match`, `=>` pertenece a `arm`, no a `lambda`: `Some(x) =>` no se parsea como lambda.

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

`STRING` admite `{IDENT}`. `{{` y `}}` son una llave literal. Cualquier otra cosa entre llaves es `E_PARSE`.

`DSTRING` es el identificador `d` inmediatamente seguido de `STRING`, sin espacio entre ambos. Es azúcar de `strdup` aplicado a esa cadena: el árbol es una llamada a `Ident("strdup")` con un argumento `STRING`, no un literal. Misma interpolación y escapes que `STRING`. Un `d` suelto o seguido de espacio sigue siendo `IDENT`.

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

### Reservado

No forman parte del arranque. El parser que las ve emite `E_PARSE` hasta su fase:

```text
attr         → "@" IDENT ("(" IDENT ")" | ε)
parallel_expr → "parallel" suite
```

`@frozen`, `@stack`, `@layout`, `@resource`, `@shared`, `@arena`, `@no_heap` y `@copy` usan `attr`. `any Protocol` no tiene producción propia: cuando exista, será un `named_type`.

## Tipos

| Tipo | Notas |
|------|--------|
| `Int` | i64 |
| `Float` | f64 (no elemento de tensor) |
| `Bool`, `String`, `Bytes`, `Unit` | |
| `List[T]` | heap, límites comprobados; `T` ∈ Int, Float, Bool, String |
| `Dict[K, V]` | heap; `K` ∈ Int, String; `V` ∈ Int, Float, Bool (copy) |
| `Tensor[Elem, dims…] on lugar` | `Elem` ∈ F32, F16, BF16, I8; `?` eje dinámico |
| structs, enums | |
| genéricos | monomorfización |

Los tipos nominales no se mezclan: `Int`, `String` y un `struct`/`enum` distinto son incompatibles salvo conversión explícita (p. ej. `.campo` en un newtype de un solo campo). `+` con `String` a la izquierda no es una excepción: exige `String` también a la derecha, y un asa `Int` se convierte explícitamente con `str_from_int` (o se formatea con `int_to_str`).

Un `struct` se construye con `Nombre(arg, …)` en el mismo orden que los campos. Un `struct` de un solo campo es un **newtype transparente**: misma representación que el campo en IR/LLVM. Un enum se construye con el nombre de variante (`None`, `Some(x)`, …); en ejecución vale un `i64` con tag en el byte bajo y payload desplazado 8 bits cuando aplica.

Lugar: `cpu`, `gpu`, `tpu` o parámetro `p`.

## Módulos

Un fichero es un módulo. `import path` enlaza las firmas de los módulos alcanzables. No se copia el cuerpo al AST del raíz. El preludio no se inyecta; hace falta `import "std/prelude.sal"` (o la ruta resuelta equivalente) para `Option`, `Result` y las primitivas documentadas allí.

`import path as alias` importa ese módulo bajo un calificador: sus `fn`, `struct` y `enum` solo son accesibles como `alias.nombre` (llamadas, tipos y patrones de variante). No se aplanan en el entorno del que importa.

Sin `as`, el importado se aplanan en transitivo como antes: `fn`, `struct` y `enum` del importado y de sus imports entran en el entorno del que importa. Además, el último segmento del path (sin `.sal`) es un calificador implícito sobre los items definidos en ese fichero (`import lexer` → `lexer.tokenize`; `import "std/prelude.sal"` → `prelude.Option`). El calificador no cubre items reexportados por transitividad. Un local o parámetro con el mismo nombre sombrea el calificador implícito.

La ruta se resuelve, en orden: desde el directorio del fichero que importa; desde la raíz del proyecto; desde los `path` de `[dependencies]` en `Sal.toml`; desde los paquetes bloqueados en `Sal.lock` (directorio en la caché global de paquetes); desde la biblioteca estándar bajo `SAL_HOME` (`std/prelude.sal`, `std/soso/…`, etc.). Un path `nombre/resto` puede enlazar un paquete cuya clave en `Sal.toml` o cuyo `name` en `Sal.lock` coincide con `nombre`. Acepta `IDENT` con `.`, o `STRING`; si falta `.sal`, se prueba añadiéndolo. Tras el path, `as` es contextual (el `IDENT` `as` justo después del path).

`Sal.toml` admite dependencias locales `path = "…"` y dependencias git `git = "…"` con `version = "…"` (requisito semver, semántica caret). `sal fetch` resuelve tags `vX.Y.Z` en el remoto, materializa cada paquete en la caché (`$SAL_PACKAGES` o `~/.sal/packages/<nombre>-<versión>/`) y escribe `Sal.lock` con `name`, `version`, `git` y `rev`. `sal build` y `sal check` no acceden a la red; si falta un paquete del lock en la caché, la resolución falla con `E_TYPE` («import not found»).

Si el mismo nombre de item choca entre módulos del grafo, el compilador asigna un símbolo de enlace único `modid__nombre` (con `modid` derivado de la ruta del módulo) solo a los items en conflicto; el resto conserva su nombre. Las referencias calificadas se resuelven a ese símbolo.

Ruta inexistente, choque de nombre entre importados aplanados o con items locales, dos `as` con el mismo alias, un alias que coincide con un item visible, referencia `alias.x` inexistente, calificador implícito ambiguo en uso, o ciclo en el grafo de imports: `E_TYPE` en el span del `import` o de la referencia.

`sal build` del módulo raíz compila cada módulo alcanzado a su objeto y enlaza esos objetos con el runtime.

## Memoria

- Stack por defecto; heap para `String`, `List`, tensores.
- Move por defecto; copy solo tipos triviales.
- Préstamo solo en argumento de llamada.
- `@frozen`, `@stack`, `@layout(c)`, `@resource` (fase posterior parcial).

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
- `bytes_to_str -> String ! alloc`, `bytes_view -> Bytes ! alloc`

`Bytes` vive en `cpu` (misma regla de lugar que `String`).

### Enteros

Precedencia (de menor a mayor): comparación, `|`, `^`, `&`, desplazamiento (`<<` `>>`), suma, producto (incluye `%`). `%` es resto con signo del dividendo (`srem`). `<<` y `>>` son desplazamiento lógico/aritmético sobre `Int` (`shl`/`ashr`); operadores bitwise solo `Int` (`E_TYPE` con `Float`).

### `syscall`

`syscall(n, a0, a1, a2, a3, a4, a5) -> Int ! io` — siete argumentos `Int`. Solo al compilar para **soso**; en host Linux es `E_TARGET`. En enlace soso baja a `sal_os_syscall`.

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

`E_MOVED`, `E_EFFECT`, `E_PLACE`, `E_SHAPE`, `E_TENSOR_ELEM`, `E_DEVICE`, `E_DEVICE_MISSING`, `E_OOM`, `E_PARSE`, `E_TYPE`.

## Instrumentación (`--instrument`)

Eventos: `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN`, `INF`.

## Autohospedaje

IR idéntica compilando el corpus con bootstrap Rust, stage1 (sal←Rust) y stage2 (sal←sal).
