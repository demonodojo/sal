# Subplan: especificación y superficie

## Entrada

Diseño maestro del lenguaje sal.

## Salida

- [SPEC.md](../../SPEC.md), con la gramática normativa.
- [README.md](../../README.md), operativo.
- Lista cerrada de códigos `E_*` y de eventos de instrumentación.

## Gramática

La definición está en [SPEC.md](../../SPEC.md). Este subplan fija cómo hay que leerla y qué árbol produce. Si este texto y la SPEC divergen, manda la SPEC.

La gramática es recursiva por la izquierda. No es un descenso recursivo escrito con producciones recursivas por la derecha. La recursión izquierda de `add`, `mul`, `cmp`, `postfix`, `named_type` y de las listas es la asociatividad izquierda. Los prefijos y la lambda se quedan recursivos por la derecha porque asocian a la derecha.

Ejemplo mínimo, el de la SPEC:

```text
add → add "-" mul | mul
```

`1 - 2 - 3` deriva por la izquierda a `(1 - 2) - 3`. El AST es `Binary { op: Sub, left: Binary(Sub, 1, 2), right: 3 }`.

Lo mismo para el resto de capas:

| Producción | Árbol de `…` |
|------------|----------------|
| `mul → mul "*" unary` | `(a * b) * c` |
| `cmp → cmp "==" add` | `(a == b) == c` |
| `postfix → postfix "." IDENT` | `(a.b).c` |
| `postfix → postfix "(" arg_list ")"` | `(f(x))(y)` |
| `named_type → named_type "[" type_list "]"` | `(List[Int])` aplicado otra vez, si hubiera otro `[…]` |
| `lambda → cmp "=>" expr` | `x => (y => z)` |

`load[F32, 4, 4]("w.salt")` usa la segunda alternativa de `postfix`. Los argumentos de tipo son `type_arg`: un tipo, un `INT` o `?`. No se codifican como `Type::Named` cuyo nombre es `"4"`.

## Superficie que el arranque tiene que aceptar

Además de `fn` e `import`:

- `struct` y `enum` con parámetros de tipo y cuerpo indentado.
- `match` con `_`, enteros e `IDENT(patrones)`.
- Lambda de un identificador: `x => expr`.
- `on lugar` y `on lugar kernel`.
- `to lugar expr`.
- `tensor[[…], […]]`.
- `try`, `if` con `elsif` y `else` opcionales, y `let` con tipo opcional.
- Tipos `Tensor[Elem, dims…] on lugar`, con `?` en un eje.

## Códigos

Compilación: `E_PARSE`, `E_TYPE`, `E_MOVED`, `E_EFFECT`, `E_PLACE`, `E_SHAPE`, `E_TENSOR_ELEM`, `E_DEVICE`, `E_DEVICE_MISSING`, `E_OOM`.

Instrumentación: `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN`, `INF`.

## Atributos en el arranque

`@frozen` y `@stack` delante de `let`; `@no_heap` delante de `fn`; `@layout(c)` delante de `struct`. Cualquier otro `@…` es `E_PARSE`. Las violaciones semánticas de un atributo admitido son `E_TYPE`.

## Fuera de arranque

`parallel`, `@resource`, `@shared`, `@arena`, `@copy`, `@unique`, `@no_gc`, diferenciación automática y el registro remoto de paquetes. Verlos en el arranque es `E_PARSE`.
