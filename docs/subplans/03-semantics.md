# Subplan: semántica

## Entrada

AST de [02-frontend-agents.md](02-frontend-agents.md).

## Salida

AST tipado, modo `borrow` o `take` de cada parámetro, efectos comprobados, diagnósticos con código y span.

## Archivos

`src/infer.rs`, `src/typed.rs`, `src/ownership.rs`, `src/effects.rs`, `src/device.rs`, `src/diag.rs`.

Tests: `tests/semantics.rs`.

## Tipos

Hindley–Milner con polimorfismo en el `let` y monomorfización en cada uso. Los genéricos de una `fn` se sustituyen al llamarla; no queda un tipo polimórfico en la IR.

Un tensor es `Tensor[Elem, dims…] on lugar`. `Elem` es `F32`, `F16`, `BF16` o `I8`. `Float` es f64 y como elemento es `E_TENSOR_ELEM`. El lugar es `cpu`, `gpu`, `tpu` o una variable `p` de la firma. Esa variable se unifica entre los parámetros y el retorno de la misma función. Si en una llamada queda libre, el `to` del sitio de llamada la cierra. Si sigue libre, `E_TYPE`.

`matmul(A, B)`: `A` es `[M, K1]`, `B` es `[K2, N]`. Si `K1` y `K2` son estáticos y distintos, `E_SHAPE`. El resultado es `[M, N]` en el lugar de los dos operandos. Si los lugares no coinciden, `E_PLACE`: el compilador no inserta un `to`.

El resto, en el mismo lugar y el mismo elemento:

| Operación | Forma del resultado |
|-----------|---------------------|
| `map`, `relu`, `softmax` | la del argumento |
| `reduce` | se elimina el eje reducido |
| `transpose` | se intercambian los dos últimos ejes |
| `reshape` | las dimensiones escritas; si todas son estáticas, el producto tiene que coincidir (`E_SHAPE` si no) |
| `to q` | la misma forma, lugar `q` |

Dos tensores del mismo elemento y la misma forma unifican eje a eje. `?` unifica con un estático y sigue siendo `?` en la firma; el estático no se filtra hacia el tipo público. Dentro de una función, un literal puede concretar el `?` para calcular tamaños, sin cambiar la firma.

`List[T]` es heap. El índice es `Int`. Structs y enums se tipan por los `Item` del módulo y del preludio. `Option` y `Result` salen del preludio, no de un caso especial con otro nombre. `try` exige un `Result` y el error tiene que coincidir con el de la función que lo envuelve. Un `match` cubre las variantes del enum; si falta una y no hay `_`, `E_TYPE`.

La lambda de una sentencia se tipa al usarla. `x => e` no es un valor de primera clase que se guarde: si se liga a un `let` o se devuelve, `E_TYPE` en este arranque. El parámetro no lleva tipo escrito; sale del callback (`map`).

Cada expresión del AST tipado guarda su tipo, su lugar y el span. `sal emit typed` imprime eso. `sal fmt` escribe en las firmas públicas el `borrow` o `take` que haya inferido el pase de ownership, para que no cambie en silencio.

## Ownership

Pase léxico, sentencia a sentencia, después de tipar. No hay lifetimes en el fuente.

- `Int`, `Float`, `Bool` y un struct cuyos campos lo son se copian.
- Lo demás es único y se mueve.
- Un préstamo dura el argumento de la llamada. No hay un local de tipo referencia.
- Mutar un campo (`user.name = …`) exige ser el dueño único.
- `to` de un valor único consume el origen. Volver a usarlo es `E_MOVED`.
- El modo del parámetro sale del cuerpo. Si solo se lee para pasarlo a una llamada, `borrow`. Si el cuerpo lo mueve, lo guarda o lo devuelve, `take`. El AST tipado publica ese modo.

`@shared` no se infiere. Si el programa necesitaría guardar un préstamo, el error pide devolver por movimiento. No se inventa un lifetime.

## Efectos y lugares

El cuerpo no puede hacer un efecto que la firma no declara (`E_EFFECT`).

| Construcción | Efectos |
|--------------|---------|
| `load` | `io`, `alloc` |
| literal `tensor`, `String`, `List` | `alloc` |
| `on gpu`, `to gpu` | `gpu` |
| `on tpu`, `to tpu` | `tpu` |
| `panic` | `panic` |

`on tpu kernel` es `E_DEVICE`. Un tensor cuyo lugar no es el del `on` que lo usa, y que no es un escalar `copy` pasado por valor, es `E_PLACE`.

## Hecho cuando

`tests/semantics.rs` falla con el código exacto:

- `Tensor[Float, 2, 2] on cpu` → `E_TENSOR_ELEM`
- `matmul` con K estáticos distintos → `E_SHAPE`
- mezcla de lugares sin `to` → `E_PLACE`
- segundo uso después de `to gpu x` → `E_MOVED`
- `on tpu kernel` → `E_DEVICE`
- `load` sin `! io, alloc` → `E_EFFECT`
- la firma tipada de un `forward` que solo lee los pesos publica `borrow` en esos parámetros
