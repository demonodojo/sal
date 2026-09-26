# Especificación del lenguaje sal (arranque)

La gramática de este documento es la definición del lenguaje. Es recursiva por la izquierda en las listas, en los operadores binarios y en los postfijos. Esa recursión es la asociatividad: la derivación izquierda de `1 - 2 - 3` es `(1 - 2) - 3`, la de `a.b.c` es `(a.b).c` y la de `f(x)(y)` es `(f(x))(y)`.

Los prefijos (`try`, `-`, `!`, `to`) y la lambda son recursivos por la derecha. `x => y => z` es `x => (y => z)` y `- - x` es `- (- x)`. Una reescritura recursiva por la derecha de los operadores no es esta gramática: cambiaría el árbol.

El lexer parte el fuente en terminales e inserta `NEWLINE`, `INDENT` y `DEDENT`. La sangría son espacios; un tabulador es `E_PARSE`. Una línea en blanco no abre un bloque. Entre líneas el parser acepta uno o más `NEWLINE`. Los comentarios `#` desaparecen en el lexer y no tienen producción.

## Superficie

- Indentación significativa; comentarios `#`.
- Bloque: última expresión es el valor; `return` opcional.
- `fn nombre(params) -> Tipo ! efectos` con efectos `io`, `alloc`, `panic`, `gpu`, `tpu`.

## Gramática

Los no terminales van en minúsculas. `ε` es la producción vacía. `IDENT`, `INT`, `FLOAT` y `STRING` son terminales del lexer. Lo demás entre comillas es terminal.

### Programa

```text
program      → item_list
item_list    → item_list item
             | ε
item         → import_def
             | struct_def
             | enum_def
             | fn_def

import_def   → "import" path
path         → path "." IDENT
             | IDENT
             | STRING

struct_def   → "struct" IDENT type_params suite_fields
suite_fields → NEWLINE INDENT field_list DEDENT
field_list   → field_list field
             | field
field        → IDENT ":" type

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

Si hay `=`, la izquierda tiene que ser un lvalue: `IDENT`, o un postfijo que solo sea una cadena de campos. Si no lo es, `E_PARSE`. La última línea que es una expresión sin `=` es el valor del bloque. Sin esa línea el bloque vale `Unit`; si la función no devuelve `Unit`, el error es `E_TYPE`.

### Expresiones

```text
expr         → "try" expr
             | lambda
lambda       → cmp "=>" expr
             | cmp
cmp          → cmp "==" add
             | cmp "!=" add
             | cmp "<"  add
             | cmp "<=" add
             | cmp ">"  add
             | cmp ">=" add
             | add
add          → add "+" mul
             | add "-" mul
             | mul
mul          → mul "*" unary
             | mul "/" unary
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

on_expr      → "on" place ("kernel" | ε) suite
match_expr   → "match" expr NEWLINE INDENT arm_list DEDENT
arm_list     → arm_list arm
             | arm
arm          → pattern "=>" expr
pattern      → "_"
             | INT
             | IDENT "(" pattern_list ")"
             | IDENT
pattern_list → pattern_list "," pattern
             | pattern
             | ε
if_expr      → "if" expr suite "else" suite
```

El parámetro a la izquierda de `=>` en `lambda` es un `IDENT`. `a + b => c` es `E_PARSE`. Dentro de `match`, `=>` pertenece a `arm`, no a `lambda`: `Some(x) =>` no se parsea como lambda.

`load[F32, 4, 4]("w.salt")` es un `postfix`: argumentos de tipo (`F32`, `4`, `4`) y luego la llamada. `4` y `?` son `type_arg`, no un tipo con el número por nombre.

`else` es obligatorio. `to gpu x + 1` es `(to gpu x) + 1`. `try x + 1` es `try (x + 1)`. `x => x * 2` es `x => (x * 2)`.

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
             | IDENT
place        → "cpu" | "gpu" | "tpu" | IDENT
```

`Tensor` seguido de `[` es `tensor_type`. Un elemento fuera de `tensor_elem` (por ejemplo `Float`) es `E_TENSOR_ELEM`.

### Cadenas

`STRING` admite `{IDENT}`. `{{` y `}}` son una llave literal. Cualquier otra cosa entre llaves es `E_PARSE`.

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
| `Bool`, `String`, `Unit` | |
| `List[T]` | heap, límites comprobados |
| `Tensor[Elem, dims…] on lugar` | `Elem` ∈ F32, F16, BF16, I8; `?` eje dinámico |
| structs, enums | |
| genéricos | monomorfización |

Lugar: `cpu`, `gpu`, `tpu` o parámetro `p`.

## Memoria

- Stack por defecto; heap para `String`, `List`, tensores.
- Move por defecto; copy solo tipos triviales.
- Préstamo solo en argumento de llamada.
- `@frozen`, `@stack`, `@layout(c)`, `@resource` (fase posterior parcial).

## Dispositivos

- `to gpu expr`, `to cpu expr`, `to tpu expr` — única transferencia (`E_PLACE` si se mezcla sin `to`).
- `on p` … bloque en dispositivo; `on gpu kernel` rechazado en tpu (`E_DEVICE`).

## Modelos

Operaciones: `matmul`, `map`, `reduce`, `softmax`, `reshape`, `transpose`, `relu`, `load`, `tensor[…]`.
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
