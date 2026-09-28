# Subplan: IR, fusión, runtime y LLVM

## Entrada

AST tipado de [03-semantics.md](03-semantics.md), con lugares, formas y modos ya comprobados.

## Salida

IR SSA, región fusionada con `peak_bytes`, runtime C, bajada a LLVM IR textual y binario nativo vía Clang.

## Archivos

`src/ir.rs`, `src/fuse.rs`, `src/llvm.rs`, `src/compile.rs`, `runtime/sal_runtime.c`, `runtime/sal_runtime.h`, `runtime/kernels.c`.

Tests: `tests/forward_ir.rs`, `tests/kernels.rs`, `tests/exec_matmul.rs`, `tests/exec_salt.rs`, `tests/run_hello.rs`.

## Bajada a SSA

Una función sal es una `IrFunction`. Cada valor intermedio es un nombre nuevo. Volver a ligar un `let` no reescribe el valor viejo: la versión nueva es otro nombre y el uso siguiente apunta a ella.

Instrucciones que la IR tiene que poder emitir, no solo `ConstInt` y `Call`:

| Instrucción | Origen |
|-------------|--------|
| `const` entero, float o cadena | literales |
| `copy` | tipo trivial |
| `move` | último uso de un único |
| `drop` | fin de ámbito de un único que sigue vivo |
| `borrow` | argumento prestado; el alcance es la llamada |
| `alloca` | local de pila |
| binario y unario | operadores de la gramática. `+` con izquierdo `String` es `sal_str_concat` (primer paso, o izquierdo `IDENT`) o `sal_str_append` (paso siguiente de la misma cadena). El derecho se pasa como el segundo argumento de esa llamada, aunque no sea `String` |
| `call` | llamada, ya monomorfizada |
| `place_copy` | `to` |
| `on_device` | bloque `on`, con su lugar |
| `matmul`, `map`, `reduce`, `softmax`, `reshape`, `transpose`, `relu` | operaciones de tensor, todavía sin fusionar |
| `tensor_bin` | `+`/`-`/`*`/`/`/cmp y `-`/`!` unarios sobre tensores de rango 1 |
| `where` | filtro de filas de un `frame` por máscara `I8` |
| `load` | `load[…](path)` |
| `return` | valor de la función |

Un struct grande vuelve por puntero oculto. Las formas estáticas monomorfizan la llamada. Un eje `?` es un entero de ejecución, un parámetro `nombre_dN` por eje.

`lower_program` de `examples/forward.sal` emite la región `on` y, si el fuente tiene `to`, un `place_copy`. `sal emit ir` muestra esas operaciones con texto estable (`fused=`, `peak=`), no el `Debug` de Rust.

## Fusión

Sobre las instrucciones de un solo `on`, en orden:

1. Varias operaciones del mismo lugar forman una región: un lanzamiento, no uno por operación. Un `to` no se fusiona; es el punto de sincronización.
2. Un `map` o `relu` cuyo único uso es el destino de un `matmul` de esa región se convierte en el epílogo de ese producto. El destino intermedio desaparece de la IR. No hay `sal_matmul` seguido de `sal_relu` como dos buffers.
3. Una cadena de `tensor_bin` del mismo lugar dentro del `on` se registra en la región fusionada; los destinos intermedios únicos no reservan buffer aparte (misma regla que el epílogo).
4. Los tensores únicos que mueren dentro de la región reutilizan un buffer de trabajo. Los parámetros `borrow` (los pesos) no se reservan otra vez.
5. `peak_bytes` por lugar es el máximo, a lo largo de la región, de la suma de bytes vivos en ese punto: pesos prestados que el cálculo necesita tener residentes, activaciones vivas y el buffer de trabajo. El tamaño de un tensor es `tam(elem) * producto(ejes)`. `F32` = 4, `F16` = 2, `BF16` = 2, `I8` = 1. Esos números salen de la forma, no de una constante fija por operación.
6. Si algún eje es `?`, `peak_bytes` de esa parte queda simbólico (`peak_symbolic`, con la fórmula) y el número publicado es solo el de los factores estáticos. No se inventa un lote.

`reduce` y la suma de `softmax` recorren el eje en orden creciente de índice. La fusión no reordena esa reducción: el mismo fuente y los mismos datos dan el mismo número.

## Runtime

El programa sal no importa este runtime. Clang lo enlaza porque el compilador lo pide.

Heaps distintos para `cpu`, `gpu` y `tpu`, aunque la máquina no tenga GPU ni TPU: `sal run` ejecuta el bloque en el heap de ese actor. Con `sal build --device gpu` y `nvcc` disponible, el enlace define `SAL_USE_CUDA`: reservas y `to gpu` usan memoria de dispositivo, y `sal_matmul_f32` en el heap gpu llama al núcleo CUDA (`runtime/gpu_matmul.cu`). Sin CUDA, el mismo contrato se emula en host.

`on gpu kernel i, j in …` baja a `KernelGrid` en la IR (bucles sobre la forma estática) y a `sal_on_enter` en dispositivo. `@shared` en el fuente sigue fuera del arranque; el mosaico gpu usa shared internamente. Hace falta reserva y liberación por lugar, `String`, `List[T]` con índice comprobado (`T` ∈ Int, Float, Bool, String), `Dict[K, V]` (`sal_dict_*`, distinto de `sal_map_*` del autohospedaje), `Tensor` con elemento y forma, `print` y `panic`.

`load` lee un `.salt`: magic `SALT`, versión u16, elem u8, rank u8, dims u64 little-endian, payload little-endian. El tensor resultante está en `cpu`. El `to` posterior es el único traslado. Si la reserva del dispositivo falla, el proceso termina con `E_OOM`: lugar, bytes pedidos y span de la región, en el JSON de diagnósticos.

`runtime/kernels.c`: `matmul` en mosaico, filas empaquetadas. `F32` y `F16` tienen núcleo; `F16`, `BF16` e `I8` acumulan en `F32`. En cpu, `BF16` e `I8` se ensanchan hacia ese núcleo. La IR conserva el tipo estrecho y el nombre de la primitiva para que otro controlador sustituya el cuerpo. `softmax` suma en orden de índice.

## LLVM

`src/llvm.rs` emite IR textual de cada función sal, no solo de `main`. Cada instrucción de la tabla de arriba tiene bajada. Las primitivas se declaran y se llaman (`sal_matmul_f32`, `sal_place_copy`, `sal_load`, …). Un bloque de dispositivo baja a la IR de dispositivo y al runtime del heap de ese actor.

`sal build` enlaza con Clang `-O0`. `sal build --release` usa `-O3`. Sin `--instrument` no hay sondas ni contabilidad de reservas.

## Hecho cuando

- La IR de `relu(matmul(x, w))` es una región, sin intermedio materializado, y `peak_bytes` sale de elemento por forma.
- Un producto 2×2 conocido coincide con el número esperado, también leído de un `.salt`.
- `sal run examples/hello.sal` termina en 42.
- El LLVM del forward declara y llama a `sal_matmul_f32` y, si hay `to`, a `sal_place_copy`.
