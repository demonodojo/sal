---
name: sal-fuente
description: >-
  Escribe y revisa programas sal canónicos: indentación, funciones, efectos,
  lugares cpu/gpu/tpu, tensores, frame, operadores columnares, sal fmt y el
  linter standard. Usar al crear o editar ficheros .sal, ejemplos, el preludio,
  el corpus, standard/ o código
  del autohospedaje. El estilo se aplica con standard; si falta una práctica
  que va a repetirse, se amplía el linter.
---

# Programas sal

La superficie aceptada está en [SPEC.md](../../../SPEC.md). Un programa que el arranque no puede parsear no se «adelanta» con sintaxis reservada.

## Forma

- Sangría de espacios. Sin tabuladores. Comentarios con `#`.
- Bloque indentado. La última expresión es el valor. Sin ella, el bloque es `Unit`.
- `fn nombre(params) -> Tipo` y, si hace falta, `!` con efectos `io`, `alloc`, `panic`, `gpu`, `tpu`.
- `if` con `elsif` y `else` opcionales, al mismo nivel de sangría. Sin `else`, el `if` es `Unit` aunque haya `elsif` (efecto secundario, no valor). Con `else`, el valor es el de la rama then. `let` con tipo opcional.
- Items del arranque: `import`, `struct`, `enum`, `frame`, `fn`, `method`, `static`, `init`.
- `match expr` con brazos `patrón => expr` o `patrón =>` bloque indentado (`_`, enteros, `"…"`, `r"…"`, `Variante(x)`, `modulo.Variante(x)`, `IDENT` ligado).
- `return` anticipado vale en cuerpo, `if`, `while` y `match`; no dentro de `on` ni de una lambda.
- Interpolación `f"a#{x}"` (`x` tiene que ser `String`; en `"…"` y `d"…"` `{` y `}` son texto).

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

## Atributos

Solo cuatro, y cada uno en su sitio; cualquier otro `@…` es `E_PARSE`:

```sal
@layout(c)
struct P
    a: Int
    b: Int

@no_heap
fn add(a: Int, b: Int) -> Int
    @stack let s = a + b
    @frozen let t = s
    t
```

`@frozen let` no se reasigna; `@stack let` exige tipo de stack y no asignar; `@no_heap fn` no asigna ni usa heap; `@layout(c)` solo con `Int`, `Float`, `Bool`. Las violaciones son `E_TYPE`. Ver [sal-autoridad](../sal-autoridad/SKILL.md).

## Métodos de tipos básicos

Los tipos básicos tienen métodos que son el builtin con el receptor primero (tabla en SPEC): `s.len()`, `xs.push(v)`, `n.to_str()`, y en `Float` `x.to_int()`, `x.to_str()`, `x.sqrt()`, `abs`, `floor`, `ceil`, `sin`, `cos`, `exp`, `log`, `x.pow(y)`. Dan el mismo IR que `float_sqrt(x)`. Los estáticos llevan argumentos de tipo: `List.new[Int]()`.

Prefiere el método a la función builtin siempre que exista. Tabla completa en la SPEC; las formas más usadas:

| En vez de | Escribe |
|-----------|---------|
| `str_len(s)` | `s.len()` |
| `str_eq(a, b)` | `a.eq(b)` |
| `list_push(xs, v)` | `xs.push(v)` |
| `list_pop(xs)` | `xs.pop()` |
| `list_get(xs, i)` / `list_set(xs, i, v)` | `xs.get(i)` / `xs.set(i, v)` |
| `list_len(xs)` | `xs.len()` |
| `int_to_str(n)` | `n.to_str()` |
| `dict_put(d, k, v)` / `dict_get(d, k)` | `d.put(k, v)` / `d.get(k)` |

`standard` reescribe esas llamadas (`Style/Method`), y el resto de la tabla de la SPEC. `str_concat` y `str_append` siguen yendo a `+`.

Esto aplica también a `selfhost/` y a los tests de `tests/lang`. Dentro de un `struct` o `enum` propio, `method T.f()` en vez de `fn f(t: T)` (ver [sal-autoridad](../sal-autoridad/SKILL.md)).

## Sentencias y variables sin uso

- Una expresión que devuelve valor puede ir como sentencia suelta: `xs.push(v)`, `g(2)`, `xs.pop()`. No hace falta ligarla.
- No asignes a una variable que no vas a usar. `_a = xs.push(v)` no aporta nada y añade un nombre que leer. El `_nombre` solo tiene sentido cuando el alias importa al pase de liberación (un `list_push` ligado a `_nombre` presta el contenedor en vez de escaparlo, ver SPEC), y en ese caso el nombre es la razón de existir.
- Un valor fresco que se descarta (`xs.pop()` de una lista dueña, `int_to_str(n)`, `xs.get(i)`, `match dict_get(d, k) …` como sentencia) se libera en esa misma sentencia: no hace falta ligarlo para soltarlo (SPEC «Liberación de locales»).
- Una función de usuario cuyo `String` de retorno es siempre fresco entrega un valor que el llamante posee: cada `return` y la cola son frescos; un `if`/`match` de nivel superior vale si **cada** rama lo es; un `while` de nivel superior la excluye. Detalle en [sal-memoria](../sal-memoria/SKILL.md). Las funciones matemáticas de `Float` toman `Float` (`Int` es `E_TYPE`) y usan libm: en `soso` son `E_TARGET`. `sal fmt` conserva los paréntesis de la base: `(a - b).abs()`. Un método inexistente es `E_TYPE`. Los métodos solo existen en el arranque (Rust): un test que los use lleva `# selfhost-xfail:`.

## Memoria y efectos

- Move por defecto. Copy solo en `Int`, `Float`, `Bool` y structs triviales.
- Un `struct` es un valor: asignar un campo en una copia o en un callee no cambia el original (`tests/lang/structs/struct_value_alias.sal`). Un struct de un solo campo es un newtype transparente.
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

## Columnas y frames

Las columnas numéricas son tensores de rango 1; los operadores aritméticos y de comparación del lenguaje actúan elemento a elemento dentro de `on` (sin sustituir `map` para lambdas arbitrarias):

```sal
fn neto(precio: Tensor[F32, ?] on p, iva: Tensor[F32, ?] on p) -> Tensor[F32, ?] on p
    on p
        precio * 1.21 + iva
```

Un **`frame`** declara el esquema de filas. Las columnas numéricas son `Tensor[Elem, ?] on p`; `String` es `List[String]` en `cpu`:

```sal
frame Ventas[p]
    precio: F32
    nombre: String

fn row(precio: Tensor[F32, ?] on p, nombre: List[String]) -> Ventas[p]
    Ventas(precio, nombre)
```

No uses `to gpu` sobre un frame que tenga columna `String` (`E_PLACE`). Pasa cada columna numérica con `to` por separado. `where(tabla, mascara)` exige máscara `Tensor[I8, ?] on p` del mismo largo de fila; el arranque expande `sal_where` en LLVM (`sal_tensor_select_*`, `sal_list_select`).

## Proyecto, imports y paquetes

Un programa con `Sal.toml` en su raíz es un **proyecto**. El preludio no se inyecta: hace falta `import "std/prelude.sal"`. La E/S de un proyecto externo está en `sal_modules/io/` y se importa `import "io/io.sal"`.

```toml
[package]
name = "mi-app"
version = "0.1.0"

[dependencies]
json = { git = "https://github.com/org/sal-json", version = "0.2" }
```

- Dependencia **local**: `path = "…"` relativo a la raíz del proyecto.
- Dependencia **git**: `version` es requisito semver (caret); `sal fetch` escribe `Sal.lock` y deja el código en la caché global. Antes del primer `check`/`build` con deps git, ejecutar `sal fetch`.
- Dependencia **GitHub**: `libx = { github = "owner/repo", version = "0.1" }` (usa releases publicadas).
- Dependencia **std**: `io = { std = "io" }` enlaza `std/io` del compilador.
- `sal install` enlaza todo en `sal_modules/<nombre>` (symlinks); un directorio real ahí se rechaza.
- `import ruta as alias` deja los items accesibles solo como `alias.nombre` (llamadas, tipos y patrones). Sin `as`, se aplanan y el último segmento del path es un calificador implícito.
- Import de paquete: `import "json/mod.sal"` o `import json.mod` cuando la clave en el manifiesto es `json`. Detalle de resolución y lock en [sal-paquetes](../sal-paquetes/SKILL.md).
- Fuera del árbol de sal, los módulos están en `sal_modules/` del proyecto (`import "io/io.sal"` → `sal_modules/io/io.sal`). El import no cambia si sal está instalado en otro sitio.

## Dónde vive cada programa

| Ruta | Para qué |
|------|----------|
| `examples/` | Programas de uso y de ejecución (p. ej. `column_cpu.sal`) |
| `corpus/` | Programas cuya IR tienen que coincidir las tres cadenas |
| `std/prelude.sal` | `Option`, `Result`, `List` y firmas de primitivas |
| `std/io/`, `std/http/`, `std/json/`, `std/text/` | Biblioteca estándar; ver [sal-stdlib](../sal-stdlib/SKILL.md) |
| `tests/lang/` | Conformidad con resultado esperado en cabecera; ver [sal-conformidad](../sal-conformidad/SKILL.md) |
| `tests/sal/` | Pruebas `test_*.sal` de `std/`, las ejecuta `sal test` |
| `selfhost/` | El compilador escrito en sal |
| `standard/` | Linter de estilo (check / fix), escrito en sal |

Las primitivas `matmul`, `softmax`, `map`, `reduce`, `reshape`, `transpose`, `relu`, `load` y `print` las resuelve el runtime. En el preludio no se reimplementan como funciones ordinarias que el usuario pueda sombrear.

## Formato vs estilo

| Herramienta | Alcance |
|-------------|---------|
| `sal fmt` | Sangría, forma canónica del AST, idempotencia sintáctica |
| `standard` | Convenciones de texto: `d"…"` en lugar de `strdup("…")`, `+` en lugar de `str_concat` / `str_append`, `"…"` (no `d"…"`) en operandos de `+`, `elsif` donde aplique la regla, y el método (`s.len()`, `xs.push(v)`, `List.new[T]()`) en lugar de la función builtin |

Orden habitual al cerrar un fichero `.sal`: `sal fmt` (stdout o editor), luego `standard check`; si hay avisos de una práctica ya cubierta, `standard fix` y revisar el diff.

El estilo se aplica con `standard`. Después de editar fuente, un segundo `fmt` no cambia nada y `standard check` no reporta infracciones en los ficheros tocados. Si el cambio sigue una práctica que ninguna regla cubre y va a repetirse, se amplía `standard` en ese mismo cambio. Ver [sal-verificar](../sal-verificar/SKILL.md) y [sal-standard](../sal-standard/SKILL.md).
