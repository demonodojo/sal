---
name: sal-fuente
description: >-
  Escribe y revisa programas sal canónicos: indentación, funciones, efectos,
  lugares cpu/gpu/tpu, tensores y el formateador. Usar al crear o editar
  ficheros .sal, ejemplos, el preludio, el corpus o código del autohospedaje.
---

# Programas sal

La superficie aceptada está en [SPEC.md](../../../SPEC.md). Un programa que el arranque no puede parsear no se «adelanta» con sintaxis reservada.

## Forma

- Sangría de espacios. Sin tabuladores. Comentarios con `#`.
- Bloque indentado. La última expresión es el valor. Sin ella, el bloque es `Unit`.
- `fn nombre(params) -> Tipo` y, si hace falta, `!` con efectos `io`, `alloc`, `panic`, `gpu`, `tpu`.
- `if` con `else` opcional. Sin `else`, el `if` es `Unit` (efecto secundario, no valor). Con `else`, el valor es el de la rama then. `let` con tipo opcional.
- Items del arranque: `import`, `struct`, `enum`, `fn`.

Ejemplo mínimo:

```sal
fn main() -> Int
    0
```

`if` sin `else` como sentencia y otro valor como cola del bloque:

```sal
fn main() -> Int
    if true
        x = 1
    0
```

Un `if` sin `else` como única cola de una función `-> T` con `T` distinto de `Unit` es `E_TYPE`.

## Memoria y efectos

- Move por defecto. Copy solo en `Int`, `Float`, `Bool` y structs triviales.
- Un valor único se usa una vez. El segundo uso es `E_MOVED`.
- El préstamo dura el argumento de la llamada. No hay locales de tipo referencia.
- `load` exige `! io, alloc` y deja el tensor en `cpu`.
- Lanzar trabajo en dispositivo es efecto: `! gpu` o `! tpu`.

## Lugares

`cpu`, `gpu` y `tpu` son actores. La única transferencia es `to lugar`.

```sal
fn scale(m: Tensor[F32, 2, 2] on p) -> Tensor[F32, 2, 2] on p
    on p
        map(m, x => x * 2.0)

fn main() -> Int ! gpu
    host = tensor[[1.0, 2.0], [3.0, 4.0]]
    dev = to gpu host
    back = to cpu scale(dev)
    0
```

- Elemento `F32`, `F16`, `BF16` o `I8`. `Float` no es elemento de tensor.
- `?` es un eje dinámico.
- Dentro de `on gpu` u `on tpu`, los agregados libres ya están en ese lugar. Los escalares entran por valor.
- `on gpu kernel` existe. `on tpu kernel` es `E_DEVICE`.

## Dónde vive cada programa

| Ruta | Para qué |
|------|----------|
| `examples/` | Programas de uso y de ejecución |
| `corpus/` | Programas cuya IR tienen que coincidir las tres cadenas |
| `std/prelude.sal` | `Option`, `Result`, `List` y firmas de primitivas |
| `selfhost/` | El compilador escrito en sal |

Las primitivas `matmul`, `softmax`, `map`, `reduce`, `reshape`, `transpose`, `relu`, `load` y `print` las resuelve el runtime. En el preludio no se reimplementan como funciones ordinarias que el usuario pueda sombrear.

Después de editar fuente, pasar el texto por `sal fmt` y comprobar que un segundo `fmt` no cambia nada. Ver [sal-verificar](../sal-verificar/SKILL.md).
