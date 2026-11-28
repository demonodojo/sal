# sal

Español | [English](README.md)

**sal** es un lenguaje de sistemas compilado a código nativo, con sintaxis por indentación, gestión de memoria sin recolector (modelo de *ownership* de Rust), **efectos explícitos en la firma** y **CPU/GPU/TPU como parte del tipo**.

El compilador emite LLVM IR y lo enlaza con Clang. Conviven dos implementaciones: el **arranque** en Rust (`src/`, el binario `sal`) y el **autohospedaje** en sal (`selfhost/`), que se usa para validar que el lenguaje se compila a sí mismo. Hay además un linter de estilo escrito en sal (`standard/`).

```sal
fn main() -> Int
    42
```

```bash
sal run examples/hello.sal   # sale con 42
```

## Tabla de contenidos

- [Ejemplo completo](#ejemplo-completo)
- [Características del lenguaje](#características-del-lenguaje)
- [Instalación](#instalación)
- [Uso del compilador](#uso-del-compilador)
- [Estructura del proyecto](#estructura-del-proyecto)
- [Estado y documentación](#estado-y-documentación)

## Ejemplo completo

```sal
import "std/prelude.sal"

struct Vec2
    x: Float
    y: Float

method Vec2.norm2(&self) -> Float
    self.x * self.x + self.y * self.y

fn apply(xs: &List[Int], f: fn(Int) -> Int) -> List[Int] ! alloc
    out = List.new[Int]()
    i = 0
    while i < xs.len()
        out.push(f(xs.get(i)))
        i = i + 1
    out

fn main() -> Int ! io, alloc
    v = Vec2(3.0, 4.0)

    xs = List.new[Int]()
    xs.push(1)
    xs.push(2)
    xs.push(3)

    doubled = apply(&xs) x => x * 2
    total = 0
    i = 0
    while i < doubled.len()
        total = total + doubled.get(i)
        i = i + 1

    label = total.to_str()
    print(f"sal: #{label}")     # sal: 12

    if v.norm2() == 25.0
        total - 12
    else
        1
```

## Características del lenguaje

### Sintaxis por indentación

Los bloques se delimitan con espacios; no hay llaves ni punto y coma. Los comentarios empiezan por `#` y un tabulador es un error de sintaxis (`E_PARSE`). La última expresión de un bloque es su valor; `return` termina la función en ese punto.

```sal
fn classify(n: Int) -> String
    if n < 0
        "negativo"
    elsif n == 0
        "cero"
    else
        "positivo"
```

### Funciones con efectos comprobados

La firma declara los efectos que la función puede realizar: `io`, `alloc`, `panic`, `gpu` y `tpu`. Llamar a una función que asigna desde otra que no declara `alloc` es un error de compilación (`E_EFFECT`), y el compilador propaga la comprobación por toda la cadena de llamadas.

```sal
fn read_name() -> String ! io, alloc
    read_file(&d"name.txt")

fn length() -> Int ! io, alloc
    read_name().len()
```

### Ownership y préstamos, como Rust

Cada valor con dueño tiene un único dueño y **se mueve por defecto**; se presta con `&T` / `&mut T` (con lifetimes y elisión como en Rust) y se copia en profundidad solo con `x.clone()`. No hay recolector, ni copias implícitas, ni modos de parámetro inferidos. La liberación se inserta al salir del ámbito y al reasignar.

```sal
fn consume(s: String) -> Int
    s.len()

fn main() -> Int ! alloc
    s = d"hola"        # d"..." es strdup: un String con dueño
    n = consume(s)     # s se mueve
    # s.len()          # error E_MOVED
    n
```

```sal
fn add_one(xs: &mut List[Int]) -> Unit
    xs.push(1)

fn main() -> Int ! alloc
    xs = List.new[Int]()
    add_one(&mut xs)
    xs.len()
```

Los valores `Copy` (`Int`, `Float`, `Bool`, `Unit` y structs solo de campos `Copy`) se copian sin mover. Las reglas de préstamo se comprueban estáticamente: dos `&` pueden coexistir, un `&mut` no se solapa con ningún otro préstamo, mutar un lugar prestado o devolver una referencia a un local es `E_BORROW`.

`@arena` es la única extensión al modelo: el heap de la llamada sale de una región que se descarta de golpe al volver, y el retorno tiene que ser de stack.

```sal
@arena
fn sum_squares(n: Int) -> Int ! alloc
    xs = List.new[Int]()
    i = 0
    while i < n
        xs.push(i * i)
        i = i + 1
    total = 0
    i = 0
    while i < xs.len()
        total = total + xs.get(i)
        i = i + 1
    total
```

### Structs, enums, métodos y constructores

Un struct se construye con el nombre y los campos en orden. Un struct de un solo campo es un *newtype* transparente, sin coste de representación. Un enum guarda su payload en caja y se consume con `match`.

```sal
struct Point
    x: Int
    y: Int

method Point.sum(&self) -> Int
    self.x + self.y

static Point.origin() -> Point
    Point(0, 0)

init Point(x: Int, y: Int)
    Point(x, y)

enum Shape
    Circle(Float)
    Square(Float)

fn area(s: &Shape) -> Float
    match s
        Circle(r) => 3.14159 * r * r
        Square(a) => a * a
```

Los tipos básicos también tienen métodos (`xs.push(v)`, `s.len()`, `List.new[Int]()`, `Dict.new[String, Int]()`), que son los mismos builtins con el receptor como primer argumento y las mismas reglas de ownership.

### Genéricos con monomorfización

Las funciones genéricas se comprueban una vez con los tipos borrados y, además, se especializan por llamada: cada combinación concreta de tipos produce su propia copia. Los structs genéricos se sustituyen igual, y los enums genéricos comparten representación.

```sal
fn id[T](x: T) -> T
    x

struct Box[T]
    value: T

fn main() -> Int
    id(7)
```

### Protocolos y despacho dinámico

Un `protocol` declara métodos requeridos y métodos provistos. Un `struct` o `enum` reclama el protocolo en su cabecera (`impl P`) o con un `impl Tipo P` externo (con regla de huérfanos), y `any[P]` da despacho dinámico por vtable.

```sal
protocol Show
    method show(self) -> String ! alloc

struct Point impl Show
    x: Int

method Point.show(self) -> String ! alloc
    self.x.to_str()

fn log(x: &any[Show]) -> String ! alloc
    x.show()
```

### Patrones, regex y lambdas

`match` elige el primer brazo que encaja. Admite variantes de enum con payload, literales de `Int` y `String`, `_` y patrones regex `r"..."`.

```sal
fn kind(line: String) -> Int
    match line
        r"^GET " => 1
        r"^POST " => 2
        _ => 0
```

Las lambdas se pasan a parámetros `fn(...)` y admiten la forma final sin paréntesis. El bloque indentado tras `=>` también vale como cuerpo.

```sal
fn fold(xs: &List[Int], initial: Int, f: fn(Int, Int) -> Int) -> Int
    acc = initial
    i = 0
    while i < xs.len()
        acc = f(acc, xs.get(i))
        i = i + 1
    acc

total = fold(&xs, 0) (a, x) => a + x
```

### Cadenas y colecciones

`String` es inmutable; `+` concatena (con efecto `alloc`) y `x = x + y` reutiliza el bloque del acumulador. Las cadenas tienen cuatro formas especiales:

| Literal | Significado |
|---------|-------------|
| `"texto"` | Literal estático en rodata (sin dueño, se comparte) |
| `d"texto"` | `strdup`: un `String` con dueño |
| `f"hola #{nombre}"` | Interpolación de variables `String` |
| `r"^x+"` | Regex POSIX ERE, solo como patrón de `match` |

`List[T]`, `Dict[K, V]` y `Bytes` son contenedores con dueño. `Dict.get` devuelve `Option[V]`, `List.get` presta el elemento y `pop` lo mueve al llamante. `Option` y `Result` viven en el preludio, que se importa explícitamente (`import "std/prelude.sal"`).

### Tensores y dispositivos en el tipo

El lugar forma parte del tipo: `Tensor[F32, 4, 4] on gpu`. `to gpu x` es una transferencia explícita y mezclar lugares sin `to` es `E_PLACE`. Las operaciones de tensor (`matmul`, `softmax`, `map`, `reduce`, `relu`, `attention`, `qmatmul`, …) se lanzan dentro de un bloque `on`:

```sal
fn scale(x: &Tensor[F32, 2, 2] on p) -> Tensor[F32, 2, 2] on p
    on p
        relu(x * 2.0)

fn main() -> Int ! gpu, alloc
    host = tensor[[1.0, 2.0], [3.0, 4.0]]
    dev = to gpu host
    _back = to cpu scale(&dev)
    0
```

Los operadores aritméticos y de comparación son **columnares** cuando un operando es un tensor: mismo elemento, mismo lugar y difusión de formas desde la derecha. Dentro de `on`, las cadenas de operadores del mismo lugar se fusionan en una sola región. Un `frame` agrupa columnas numéricas o `String` con un eje de fila y se filtra con `where(tabla, máscara)`.

### Módulos y paquetes

Cada fichero es un módulo. `import path` aplana los items del módulo (y de sus imports); `import path as alias` obliga a calificar con `alias.nombre`.

```sal
import "std/io/fs.sal"
import "mi/proyecto/matematicas.sal" as mat
```

Las dependencias se declaran en `Sal.toml` y se resuelven con `sal fetch` / `sal install` (que enlazan cada paquete en `sal_modules/`). Se admiten dependencias `path`, `git` con versión semver y releases de GitHub (`{ github = "owner/repo", version = "0.1" }`). El lockfile `Sal.lock` fija la revisión.

### Atributos

| Atributo | Delante de | Efecto |
|----------|-----------|--------|
| `@frozen` | `let` | El binding no se reasigna |
| `@stack` | `let` | El valor es de stack y construirlo no asigna |
| `@no_heap` | `fn` | La función no toca el heap |
| `@arena` | `fn`, `method`, `static`, `init` | Heap por región, retorno de stack |
| `@layout(c)` | `struct` | Representación C (`Int`, `Float`, `Bool`) |
| `@deprecated("texto")` | item | Aviso `W_DEPRECATED` en cada uso |

### Diagnósticos con códigos cerrados

Los errores llevan un código estable, y `--error-format json` los emite para herramientas:

| Código | Significado |
|--------|-------------|
| `E_PARSE` | Sintaxis (incluye lo reservado para fases futuras) |
| `E_TYPE` | Tipos, aridad, nombres o imports |
| `E_MOVED` | Uso de un valor después de moverlo |
| `E_BORROW` | Préstamo solapado, referencia que sobrevive a su referente, lifetime sin origen |
| `E_EFFECT` | La función realiza un efecto que no declara |
| `E_PLACE` | Mezcla de lugares (`cpu`/`gpu`/`tpu`) sin `to` |
| `E_SHAPE` | Formas de tensor incompatibles |
| `E_TENSOR_ELEM` | Elemento de tensor no admitido |
| `E_DEVICE` | Operación no válida en ese dispositivo |
| `E_DEVICE_MISSING` | Dispositivo no disponible |
| `E_OOM` | Sin memoria |
| `E_TARGET` | La construcción no existe en el destino (`--target`) |

`E_INTERNAL` queda para fallos del compilador. `W_DEPRECATED` es un aviso y no interrumpe la compilación.

### Instrumentación y comprobación de memoria

Con `--instrument`, el binario incluye sondas para `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN` e `INF`. Con `--sanitize`, el programa se compila y enlaza con ASan, LSan y UBSan para detectar fugas, dobles liberaciones y uso tras liberar.

## Instalación

Requisitos: Rust (edición 2021) y Clang.

```bash
cargo build --release
```

Quedan en `target/release/`:

- `sal` — compilador (arranque)
- `standard` — linter de estilo escrito en sal
- `som2salt` — conversor de tensores

## Uso del compilador

```bash
sal check examples/hello.sal      # solo comprueba
sal build examples/hello.sal      # compila y enlaza (target/sal-out/a.out)
sal run examples/hello.sal        # compila y ejecuta
sal fmt examples/hello.sal        # formatea
sal emit ast examples/hello.sal   # AST en JSON
sal emit ir examples/forward.sal  # IR
sal test                          # tests/sal/test_*.sal
sal selfhost                      # stage1 y stage2 del autohospedaje
sal parity examples/forward.sal   # compara la IR del arranque con la del selfhost
```

Flags globales y por comando:

| Flag | Efecto |
|------|--------|
| `-o, --output FILE` | Ruta del binario generado |
| `--release` | Optimización y LTO completo (módulos y runtime como bitcode) |
| `--device cpu\|gpu\|tpu` | Dispositivo por defecto de la compilación |
| `--target linux\|soso` | Destino de compilación |
| `--instrument`, `--instrument-out FILE` | Sondas de instrumentación |
| `--sanitize` | ASan + LSan + UBSan (no disponible para `soso`) |
| `--error-format human\|json` | Formato de los diagnósticos |

Dependencias:

```bash
sal fetch            # descarga dependencias y escribe Sal.lock
sal install          # enlaza las dependencias en sal_modules/
sal add nombre --git URL --version 0.1
```

Linter de estilo:

```bash
standard check examples/hello.sal
standard fix examples/hello.sal   # reescribe el fichero in situ
```

## Estructura del proyecto

| Ruta | Contenido |
|------|-----------|
| `src/` | Compilador de arranque en Rust (lexer, parser, inferencia, ownership, efectos, drops, IR, fusión, LLVM, paquetes) |
| `runtime/` | Runtime en C (kernels, instrumentación, destino `soso`) |
| `selfhost/` | Compilador escrito en sal (espejo de los pases del arranque) |
| `std/` | Preludio y biblioteca estándar en sal (`std/io`, `http`, `json`, `text`, `crypto`) |
| `standard/` | Linter de estilo, escrito en sal |
| `corpus/`, `examples/` | Programas de paridad y ejemplos |
| `tests/` | Tests en Rust, pruebas de conformidad (`tests/lang/`) y tests de `std` (`tests/sal/`) |
| `editors/vscode/` | Extensión de VS Code |

## Estado y documentación

La especificación del lenguaje —gramática, tipos, memoria, módulos, tensores— es **[SPEC.md](SPEC.md)** y manda sobre la implementación. Los subplanes por pase están en [docs/subplans/](docs/subplans/). Las convenciones de estilo y diseño del código sal (structs con métodos, un módulo por struct, protocolos, lambdas) están en [docs/buenas-practicas.md](docs/buenas-practicas.md).

El compilador de arranque ejecuta hoy los pases heredados de ownership y drops mientras se cierra el subplan 13; el autohospedaje valida el lenguaje, pero no sustituye al arranque. Una construcción marcada con `# xfail` en `tests/lang/` es un hueco conocido, no una característica soportada.

Tests:

```bash
cargo test --locked                # puerta completa de release
cargo test --test lang             # conformidad del lenguaje
cargo test --test selfhost_ir -- --test-threads=1
cargo test --test standard -- --test-threads=1
```
