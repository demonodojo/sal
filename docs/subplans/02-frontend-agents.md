# Subplan: frontend y agentes

## Entrada

La gramática recursiva por la izquierda de [SPEC.md](../../SPEC.md).

## Salida

Lexer, parser cuyo AST es el de esa gramática, spans, formateador idempotente y JSON del AST.

## Archivos

`src/lexer.rs`, `src/parser.rs`, `src/ast.rs`, `src/fmt.rs`, `src/span.rs`.

Tests: `tests/frontend.rs`, `tests/parse_and_fmt.rs`.

## Lexer

El lexer no conoce la gramática de expresiones. Emite tokens e inserta layout:

1. `#` hasta fin de línea se descarta, sin token.
2. Un salto de línea emite `NEWLINE`. Si la línea siguiente está vacía, no hay `INDENT` ni `DEDENT`.
3. La sangría se mide en espacios. Si la columna sube, un `INDENT`. Si baja, tantos `DEDENT` como niveles desapilados. Si no coincide con un nivel abierto, `E_PARSE`.
4. Un tabulador es `E_PARSE`.
5. `->` es `Arrow`, `=>` es `FatArrow`, `==` `!=` `<=` `>=` son un token cada uno.
6. Las palabras clave son las de la SPEC (`fn`, `let`, `if`, `elsif`, `else`, `match`, `on`, `to`, `kernel`, `return`, `import`, `try`, `struct`, `enum`, `cpu`, `gpu`, `tpu`, `true`, `false`). `borrow`, `take`, `io`, `alloc` y `panic` son `IDENT` que el parser reconoce en su sitio. `F32`, `F16`, `BF16` e `I8` también.
7. `STRING` guarda el texto y, si hay `{IDENT}`, las partes `Lit` e `Interp`. `{{` y `}}` son escape. Otra interpolación es `E_PARSE`.
8. Si un `IDENT` es exactamente `d` y el siguiente carácter es `"`, el lexer emite un solo token `DupString` (gramática `DSTRING`) con el contenido de la cadena; el span va desde la `d` hasta la comilla de cierre. El parser lo desazucará a `Call { func: Ident("strdup"), args: [String …] }`.
9. El lexer cuenta `(` y `[` sin cerrar, fuera de cadenas y comentarios. Con esa profundidad mayor que cero, un salto de línea emite `NEWLINE` y no mide sangría: no hay `INDENT` ni `DEDENT`, y una sangría distinta no es `E_PARSE`. Al cerrar, la profundidad no baja de cero. El parser, con la misma profundidad, ignora `NEWLINE` antes de un operador, un operando, una coma o el cierre. Fuera, un salto de línea sigue terminando la sentencia.

Al final del fichero se cierran los `DEDENT` pendientes y se emite `EOF`.

## Parser

El parser implementa la gramática de la SPEC, no otra. Cada producción recursiva por la izquierda se escribe como un bucle que crece el hijo izquierdo. El operando derecho es el no terminal de la capa siguiente, nunca el mismo: si el derecho volviera a llamar a `add`, la suma asociaría a la derecha y dejaría de ser esta gramática.

`add → add "+" mul | add "-" mul | mul` queda así:

```text
parse_add():
    left = parse_mul()
    mientras el token sea "+" o "-":
        op = consumir
        right = parse_mul()
        left = Binary(op, left, right)
    devolver left
```

El mismo esquema, con su conjunto de operadores, para `cmp` y `mul`. `postfix` también:

```text
parse_postfix():
    left = parse_primary()
    bucle:
        "("            → left = Call(left, tipos vacíos, args)
        "[" tipos "]" "(" args ")" → left = Call(left, tipos, args)
        "." IDENT      → left = Field(left, nombre)
        si no, salir
    devolver left
```

`parse_unary` es recursivo por la derecha: menos, `!` y `to lugar` llaman otra vez a `parse_unary`. `parse_expr` reconoce `try` y vuelve a llamarse. `parse_lambda` parsea un `cmp`; si viene `=>` y la izquierda es un `IDENT`, el cuerpo es un `expr` completo (puede llevar otro `try` o otra lambda).

Los `=>` de un `match` no pasan por `parse_lambda`. Se leen en `arm`, con el parser ya dentro del `match`.

`if_expr` lee la condición, la suite, y luego la lista `elsif` en un bucle (cada `"elsif" expr suite` se añade al final). Después, si el token es `else`, una suite más. `elsif` y `else` quedan al nivel del `if` que acaba de cerrar su suite. En el AST, `Expr::If.elsifs` es esa lista y `else_block` sigue siendo `Option<Block>` (`None` sin `else`).

Listas (`item_list`, `param_list`, `arg_list`, `field_list`, `arm_list`, `row_list`, `dims`): un elemento y, mientras haya separador, otro elemento a la derecha. El árbol de lista crece por la izquierda, igual que la producción `xs → xs "," x | x`.

Asignación: una línea `expr "=" expr` solo es `Stmt::Assign` si la izquierda es un lvalue (`IDENT` o solo campos). Si no, `E_PARSE`.

`Type::Named { name: "4" }` no representa un eje. El AST de un `type_arg` distingue:

- `TypeArg::Type(Type)`
- `TypeArg::Dim(Dim)` para `INT` y `?`

`load[F32, 4, 4]("w.salt")` guarda `F32` como tipo y `4`, `4` como dimensiones. Sube `AST_SCHEMA_VERSION` a 2 y el JSON lo dice en `schema_version`.

Un `@` o la palabra `parallel` en el arranque es `E_PARSE`.

## Formateador

`sal fmt` imprime el árbol, no el texto original. Una segunda pasada sobre esa salida devuelve los mismos bytes. Imprime lambdas, `struct`, `enum`, `on`, `to`, `tensor`, `borrow` y `take`. No reordena operadores: el árbol izquierdo se imprime izquierdo, así que `1 - 2 - 3` no se reasocia.

## Tests

En `tests/frontend.rs` y `tests/parse_and_fmt.rs`:

1. `1 - 2 - 3`, `a * b * c` y `a == b == c` tienen el hijo izquierdo compuesto.
2. `a.b.c` es `Field(Field(a, b), c)`. `f(x)(y)` es `Call(Call(f, …), …)`.
3. `x => y => z` es lambda cuyo cuerpo es otra lambda. `x => try x` parsea.
4. `map(m, x => x * 2.0)` parsea, incluido `examples/gpu_roundtrip.sal`.
5. `load[F32, 4, 4]("w.salt")` tiene dos `TypeArg::Dim`, no dos tipos llamados `"4"`.
6. `enum Option[T]` / `Result[T, E]`, `match Some(x) =>`, `struct` con campos.
7. `format(parse(s))` dos veces es idéntico en `hello`, `forward` y `gpu_roundtrip`.
8. Sangría rota y tabulador → `E_PARSE`.
9. Ida y vuelta JSON del AST con `schema_version` 2.

## Fuera de este subplan

Tipar la lambda, bajarla a IR y escribir los cuerpos de `std/prelude.sal`.
