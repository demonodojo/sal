# Buenas prácticas de sal

Guía de estilo y diseño para programas y bibliotecas en sal. No amplía el lenguaje: la
autoridad es [SPEC.md](../SPEC.md) y, en el compilador, [AGENTS.md](../AGENTS.md). El linter
`standard` automatiza una parte de estas reglas; lo que no cubre se revisa a mano.

La idea que resume el resto: **el tipo manda**. Un dato y las operaciones que lo conservan
viven juntos, con nombre propio y en su fichero; lo que comparten varios tipos se escribe una
vez en un protocolo; y lo que cambia de una llamada a otra se pasa como lambda en vez de
duplicar el bucle.

## 1. Structs con métodos, no funciones sueltas

Un `struct` es el sitio natural de su comportamiento. Si una función toma un tipo como primer
argumento y hace algo con él, casi siempre es un `method`; si construye o devuelve un tipo, es
un `static` o un `init`.

```sal
struct Vec2
    x: Float
    y: Float

static Vec2.zero() -> Vec2
    Vec2(0.0, 0.0)

method Vec2.plus(&self, other: &Vec2) -> Vec2
    Vec2(self.x + other.x, self.y + other.y)

method Vec2.length(&self) -> Float
    (self.x * self.x + self.y * self.y).sqrt()
```

Se lee mejor `v.plus(&w)` que `vec2_plus(v, w)`, y el receptor deja claro qué se está
modificando: `&self` para leer, `&mut self` para escribir, `self` solo cuando el método
consume el valor. El receptor no es decorativo: `self` mueve, `&self` presta y `&mut self`
presta en exclusiva. Elegir mal el receptor cambia el ownership de todo el que llama.

Reglas prácticas:

- Un tipo propio del módulo: `method`/`static`. Los tipos básicos ya tienen métodos
  (`s.len()`, `xs.push(v)`, `List.new[Int]()`); no se redefinen ni se amplían.
- Un `static` es la forma de un constructor con nombre o de una factoría:
  `Path.new(s)`, `Bytes.view_mem(ptr, n)`, `Vec2.zero()`.
- `init Tipo(...)` vale cuando el constructor es uno solo y se llama `Tipo.new(...)`.
- No se definen métodos para tipos de otro módulo. Se definen funciones en el módulo del tipo,
  o un protocolo (ver §3).
- Una función libre sigue siendo correcta cuando no hay receptor: utilidades de un pase,
  envoltorios de `syscall`, operaciones sobre varios tipos heterogéneos o el cuerpo compartido
  de un protocolo. En esos casos va **al final** del fichero, por debajo del tipo.

`standard` ya reescribe la llamada al builtin cuando existe el método (`Style/Method`) y traduce
la API antigua de `std/io` a métodos (`Style/StdApi`):

```sal
# mal
n = str_len(s)
list_push(xs, v)
cwd = getcwd_str()

# bien
n = s.len()
xs.push(v)
cwd = Path.cwd()
```

No cambies la API pública de golpe: deja el envoltorio antiguo marcado con `@deprecated` y el
mensaje que dice el reemplazo, como hace `std/io/fs.sal`. Así el código existente sigue
compilando y el aviso `W_DEPRECATED` guía la migración.

## 2. Un módulo por struct, agrupado por función

Cada `struct` principal tiene su fichero, con el nombre del struct en `snake_case`
(`TypeEnv` → `type_env.sal`). Si el fichero nombra una responsabilidad completa en vez de un
tipo (`std/io/fs.sal` contiene `Path`), manda el nombre de la responsabilidad y el fichero
sigue agrupando un solo tema. Dentro del fichero, en este orden:

1. el `struct` y sus `method`/`static`;
2. las constantes del módulo;
3. las funciones auxiliares privadas, al final.

```text
std/io/
  abi.sal        constantes y syscalls
  fs.sal         struct Path
  net.sal        estructura de red
  process.sal    procesos
  time.sal       tiempo
  ...
std/text/
  glob.sal
  pattern.sal
  strutil.sal
```

Los ficheros se agrupan **por función**, no por capa ni por tamaño: `std/io` reúne lo que
habla con el sistema operativo, `std/text` lo que procesa cadenas, `std/http` lo que habla
HTTP, `std/json` lo que lee y escribe JSON. Si un directorio mezcla responsabilidades, el
siguiente que lo lea no sabrá dónde poner su fichero.

Un struct por fichero es la norma. La excepción estrecha es el struct auxiliar que solo existe
para devolver varios valores de una función interna y no tiene vida propia; entonces puede
compartir fichero con su único consumidor (es lo que hacen los pases de `standard/`, un
fichero por cop). Lo que no vale es meter dos tipos públicos no relacionados en el mismo
fichero ni llamar `utils.sal` a un cajón de sastre.

Un módulo con manifiesto (`Sal.toml`) se importa por nombre; un fichero suelto, por ruta
relativa. Los métodos de un tipo importado se llaman igual (`p.size()`), aunque no se puedan
definir fuera de su módulo.

## 3. Protocolos en lugar de código duplicado

Si dos tipos comparten un algoritmo —recorrer, mapear, validar, serializar—, el algoritmo va
en un `protocol` como método **provisto** y cada tipo solo declara lo que de verdad cambia.
No copies el bucle en cada tipo.

```sal
protocol Shape
    method area(&self) -> Float
    method describe(&self) -> String ! alloc
        "area=" + self.area().to_str()

struct Circle impl Shape
    r: Float

method Circle.area(&self) -> Float
    3.14159 * self.r * self.r

struct Square impl Shape
    side: Float

method Square.area(&self) -> Float
    self.side * self.side
```

En un proyecto real cada struct vive en su fichero; aquí van juntos para que se vea el reparto.

`Circle` y `Square` heredan `describe` sin escribirlo. La cabecera `struct Circle impl Shape`
declara el compromiso y el compilador comprueba que no falta ningún método requerido: la firma,
la aridad, los efectos y el receptor tienen que coincidir. Si un tipo necesita otro
comportamiento, redefine el método provisto con la misma firma y prevalece.

Pautas:

- Deja como **requeridos** los métodos mínimos que el algoritmo necesita (`area`); escribe como
  **provistos** los que se deducen de ellos (`describe`, `map`, `filter`, `reduce`, `each`).
- Un `static` de protocolo sirve para factorías que deben existir en todos los tipos que lo
  implementan; dentro del protocolo, `Self` es el tipo que implementa.
- Respeta la regla de huérfanos: el `impl` va en el módulo del tipo o en el del protocolo.
- Un protocolo genérico (`protocol Collection[T]`) evita repetir la misma implementación para
  cada tipo de elemento. En el preludio, `Collection[T]` define `len`/`get`/`set`/`push`/`pop`
  y regala `each`, `map`, `reduce`, `filter` y `any` a todo lo que lo implemente.
- `any[P]` es para despacho dinámico de verdad: encajona el valor y exige `alloc`. Si el tipo
  se conoce en la llamada, prefiere un parámetro genérico o un método normal; no pagues una
  caja y una vtable por comodidad.

El patrón completo está en `std/prelude.sal`:

```sal
protocol Collection[T]
    method len(&self) -> Int
    method get(&self, i: Int) -> &T
    method push(&mut self, value: T) -> Unit
    method each(&self, f: fn(&T) -> Unit) -> Unit
        i = 0
        while i < self.len()
            f(self.get(i))
            i = i + 1
    method map[U](&self, f: fn(&T) -> U) -> List[U] ! alloc
        out = List.new[U]()
        i = 0
        while i < self.len()
            out.push(f(self.get(i)))
            i = i + 1
        out
```

## 4. Lambdas: pasa la operación, no repitas el recorrido

Una lambda expresa la parte que cambia de una llamada a otra. Si el protocolo ya ofrece
`each`, `map`, `filter` o `reduce`, no escribas el `while` a mano.

```sal
import "../../std/prelude.sal"

fn main() -> Int ! alloc
    xs = List.new[Int]()
    xs.push(1)
    xs.push(2)
    xs.push(3)
    ys = xs.map x => x * 10
    ys.reduce(0) (acc, y) => acc + y
```

Formas que admite el lenguaje, dentro de una función:

```sal
xs.each x =>
    print_str("x")

xs.reduce(0) (acc, x) =>
    acc + x
```

- Lambda de un parámetro: `x => expr`. De varios: `(a, b) => expr`. Con bloque indentado, el
  valor es la última expresión del bloque.
- Lambda final: el último argumento puede quedar fuera del paréntesis
  (`xs.map x => x * 2`, `xs.reduce(0) (a, x) => a + x`). El bloque cierra la llamada.
- Las lambdas **no son valores**: se pasan a un parámetro `fn(...)` o se ligan a un local como
  macro; no se guardan en un struct ni se devuelven. El compilador especializa la llamada y
  eleva la lambda a una `fn` propia; las capturas van por referencia, o por valor si son `Copy`.
- Dentro de una lambda no hay `return` ni `break` (`E_TYPE`). Si necesitas salir antes,
  reestructura con `filter`/`any` o devuelve un valor.
- Sé explícito con los efectos: un `map` que construye una `List` lleva `! alloc`; iterar con
  `each` no lo necesita.

## 5. El resto del estilo que ya corrige `standard`

`standard check fichero.sal` señala; `standard fix fichero.sal` reescribe. Todo `.sal` tocado
debería pasar `standard check`. Reglas actuales:

| Cop | Qué corrige |
|-----|-------------|
| `Style/Method` | Llamada a builtin que ya tiene método: `str_len(s)` → `s.len()` |
| `Style/StdApi` | API antigua de `std/io`: `read_fd(fd, …)` → `Fd.new(fd).read(…)` |
| `Style/ConstFn` | `fn`/`method`/`static` en mayúsculas con cuerpo constante → constante de módulo; sus llamadas pierden `()` |
| `Style/DupString` | `strdup("…")` → `d"…"` |
| `Style/StringPlus` | `str_concat`/`str_append` → `+` |
| `Style/MatchIf` | Cadena `if`/`elsif` homogénea → `match` |
| `Style/Elsif` | `else` + `if` anidado → `elsif` |
| `Style/And` | `if` anidado con el mismo `else` → `&&` |
| `Style/PlusLiteral` | `d"…"` dentro de una suma `+` → literal normal |
| `Style/UnusedBinding` | `x = llamada()` con `x` sin usar → la llamada como sentencia |

Si una práctica se repite y ninguna regla la cubre, la solución es ampliar `standard` en el
mismo cambio (cop, `run_all_cops`, fixtures `_in`/`_out` y test en `tests/standard.rs`), no
confiar en que cada revisor lo recuerde.

## 6. Memoria y efectos: explícitos, como en Rust

El modelo es el de Rust, más `@arena`:

- Un valor con dueño se **mueve** por defecto. Copiarlo es `x.clone()`, que declara `alloc`.
  No hay copias implícitas ni heurísticas de valor «fresco».
- `&T` y `&mut T` prestan, con lifetimes y elisión de Rust. Un préstamo que se solapa o que
  sobrevive a su referente es `E_BORROW`.
- Cada valor se libera al salir de su ámbito con la glue de su tipo. No escribas liberaciones
  a mano ni añadas lógica de memoria a los pases heredados.
- `! alloc`, `! io`, etc. se declaran en la firma y se propagan a quien llama. Una función que
  reserva sin declararlo es un error de tipos, no un detalle de estilo.
- `d"…"` cuando necesitas un `String` con dueño; el literal normal cuando basta prestado.
- `@arena` es la única extensión del modelo; no la uses como sustituto de pensar el ownership.

## 7. Tests y conformidad

- Un comportamiento nuevo del lenguaje se prueba en `tests/lang/<área>/*.sal`, con el
  resultado esperado escrito **a mano** en la cabecera (`exit`, `stdout`, `error`), según la
  SPEC. No se copia de la salida del compilador.
- Un hueco conocido se marca con `# xfail: motivo` (arranque) o `# selfhost-xfail: motivo`
  (autohospedaje). Al cerrarlo, se quita la marca en el mismo cambio.
- Una fuga conocida se marca `# asan-xfail: motivo`; los cambios de memoria no están hechos
  hasta que pasa `tests/memory_asan.rs`.
- Los diagnósticos se afirman con su código exacto (`E_TYPE`, `E_MOVED`, `E_BORROW`, …); no se
  inventan códigos nuevos.

## 8. Checklist antes de dar un fichero por terminado

- [ ] ¿El tipo y sus operaciones están en el mismo fichero, con `method`/`static`?
- [ ] ¿El fichero tiene nombre propio, agrupa un solo tema y vive en la carpeta de su función?
- [ ] ¿Hay funciones libres que deberían ser métodos, o métodos que deberían ser funciones?
- [ ] ¿Hay dos tipos repitiendo el mismo algoritmo que podría ser un método provisto?
- [ ] ¿Los bucles de recorrido pueden ser `each`/`map`/`filter`/`reduce` con lambda?
- [ ] ¿Los receptores (`self`, `&self`, `&mut self`) y los efectos son los mínimos correctos?
- [ ] ¿`standard check` pasa sin avisos y `sal fmt` no cambia nada?
- [ ] ¿El test del área pasa y, si es lenguaje, hay caso en `tests/lang` con expectativa a mano?
