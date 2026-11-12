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
| `const` entero, float o cadena | literales, y un uso de constante de módulo que no está sombreado por un local (`ConstInt` / `ConstFloat` / `ConstString`; un `Bool` es `ConstInt` 0/1) |
| `copy` | tipo trivial |
| `move` | último uso de un único |
| `drop` | fin de ámbito de un local fresco que sigue vivo. `String`, `List` (descriptor `i`/`s`/`E`/`S(…)` vía `sal_list_new_desc`; `list_new_owned` y prefijo `+` marcan dueño de elementos), `Dict` (kinds en `sal_dict_new`: escalares, `String`, enum, struct empaquetado), caja de enum, `Bytes` (incl. cabecera de `bytes_view`), literal `tensor` (`sal_tensor_free` / `sal_place_free`), copia completa de struct (`sal_struct_free`) y **solo la caja** de una copia cuyos campos se escalarizaron (`sal_struct_free_box`). `dict_get` → `Option` fresco (`enum_free`). Cola de struct en `lower_fn`: `sal_free`, `sal_enum_free`, `sal_list_free` (incl. `List[P]` describible), contenedores y anidados (SPEC). Pase `insert_drops` en `src/drops.rs` / `selfhost/ir/drops.sal` reescribe el AST antes de bajar; detalle en [03-semantics.md](03-semantics.md) y SPEC |
| `borrow` | argumento prestado; el alcance es la llamada |
| `alloca` | local de pila |
| `&&` y `||` | no son un `binary`: se bajan como un `if` (`a && b` → `if a then (b != 0) else false`; `a || b` → `if a then true else (b != 0)`), así que el operando derecho solo se evalúa en la rama que lo necesita. `b != 0` normaliza un `Int` a `0`/`1`. Se intercepta en `lower_expr` antes del caso general de `Binary` |
| binario y unario | operadores de la gramática. Una cadena de `+` con izquierdo `String` (los dos operandos son `String`, SPEC) se aplana en las hojas de su espina izquierda, con los `STRING` adyacentes ya plegados, y es una sola llamada: `sal_str_concat(a, b)` con dos hojas o `sal_str_concat_n(a, b, c, …)` con más (los backends pasan `n` y un arreglo de punteros en la pila). En el acumulador `x = x + …` (destino = hoja izquierda, `x` un `String` con dueño y no una referencia) es `sal_str_append(x, b)` o `sal_str_append_n(x, b, c, …)`, que consumen `x` y pueden reutilizar su bloque. Las hojas se evalúan de izquierda a derecha antes de la llamada; después se sueltan con `sal_free` las hojas temporales con dueño (`add_right_is_fresh_temp`, y la primera si es `add_left_is_fresh_call` y no hay acumulador). Ver [immutable-strings.md](immutable-strings.md) |
| `match` | Ver tabla siguiente (`lower_match_chain` / `lower_match_switch` en `src/ir.rs`) |
| `call` | llamada, ya monomorfizada |
| `place_copy` | `to` |
| `on_device` | bloque `on`, con su lugar |
| `matmul`, `map`, `reduce`, `softmax`, `reshape`, `transpose`, `relu` | operaciones de tensor, todavía sin fusionar |
| `tensor_bin` | `+`/`-`/`*`/`/`/cmp y `-`/`!` unarios sobre tensores de rango 1 |
| `where` | filtro de filas de un `frame` por máscara `I8` |
| `load` | `load[…](path)` |
| `return` | termina la función con el valor; no se baja ni se ejecuta lo posterior en la misma lista |

### `match` (`lower_match_chain`, `match_should_use_switch`)

- **Patrones literales** (`INT`, `STRING`, `RSTRING`): cadena de `If` en orden de fuente; condición `eq` (enteros), `sal_str_eq` (cadenas) o `sal_regex_match` (regex POSIX ERE → `0`/`1`; runtime `sal_regex_match` en `sal_runtime.c`). El escrutado no se mueve por el patrón.
- **Patrones de enlace** (`IDENT` que no es variante, `_`): condición siempre verdadera (`ConstInt 1`) y el nombre liga al escrutado; **no** usar `Switch` con tag ficticio sobre `String` (los brazos restantes solo con `_`/`IDENT` sobre `String` o `Int` siguen en cadena de `if`, salvo la cola entera de un `match` de enteros solo con `_`, que puede ir a `Switch` con `default`).
- **`Switch` sobre tag**: cuando `match_should_use_switch` lo permite: cola (o match entero) formada solo por `Variant` / variante unitaria (`IDENT`) / `_`, y escrutado enum conocido, **o** cualquier cola que incluya al menos un `Variant`, **o** cola solo `_`/`IDENT`/`Variant` sobre `Int`. Escrutado `String` nunca entra en `Switch`.
- **Cuerpo de brazo bloque**: `Expr::Block` en el AST tipado; `lower_expr` ejecuta sentencias del bloque y usa la cola (o `0` si falta).

Tests: `tests/forward_ir.rs` (`match_string_pattern_*`, `match_regex_pattern_*`), `tests/lang/control_flow/match_*.sal`, `tests/soso_target.rs` (`E_TARGET` para `r"…"` en soso).

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

### Caja de la copia que toma posesión

`sal_list_get` devuelve el puntero guardado (`String`, enum o struct describible). No llama a `desc_copy`. Un struct se mueve: pasarlo a una función que solo lee campos, o que los recompone (`P(p.name, p.n)`), no clona. Ese resultado es un alias (los mismos punteros); la cola no suelta sus hijos. El pase de drops inserta `desc_copy` (baja a `sal_desc_copy`) solo para una segunda dueña: `return` del valor cuando su destino vive más que quien posee los hijos, el parámetro mismo guardado o devuelto (`list_push(ys, p)`, `return p`), `list_push` / `list_set` / `dict_put`, y el payload de una variante. Un constructor armado solo con campos prestados no copia. `p.x = v` sobre un elemento de lista escribe el hueco del bloque (`sal_struct_set`); no reserva otro. `list_pop` sigue moviendo el elemento.

Después de `fuse_function` en cada `IrFunction`, `finish_module_box_shell_drops` (`src/ir.rs`, invocado desde `fuse_module` / `fuse_module_opts` en `src/fuse.rs`) recorre la IR ya fusionada:

1. Destinos de `sal_desc_copy` cuyo descriptor empieza por `S`, y de `sal_list_pop`, en `loop_depth == 0` quedan registrados como cajas de struct.
2. Si esa caja es el primer argumento de una llamada de usuario (no `sal_*`, salvo `list_push` / `list_set` / `dict_put`, que transfieren la caja), se marca para `sal_struct_free_box`.
3. Se inserta `Call { func: "sal_struct_free_box", args: [copia] }` **antes** del último `Return` de la función (nunca después del retorno). Un `sal_desc_copy` de struct que es el valor devuelto lo suelta la escalarización después de copiar los campos al `sret`. Si esa función es `@arena`, `sink_arena_leaves` deja `sal_arena_leave` inmediatamente antes de cada `Return`, después de ese `free`: un `free` posterior al `leave` llamaría a `free` de libc sobre memoria ya devuelta.

La misma regla no se aplica al compilar objetos de módulos cuyo path contiene el componente `selfhost` (`compile_file` → `fuse_module_opts(..., false)`), para evitar liberaciones al final de función sobre SSA definido dentro de bucles del propio compilador. `sal emit ir` y `bootstrap_ir_text` llaman `fuse_module` con el pase activo.

Tests de IR: `tests/drops.rs` (`lower_program` + `fuse_module` antes de `ir_to_text`); ejecución estricta: `tests/instrument_sal.rs`.

## Runtime

El programa sal no importa este runtime. Clang lo enlaza porque el compilador lo pide.

El asignador de `String`, `vec` y `map` es `malloc`/`free` (en soso, el `malloc` de la plataforma), salvo dentro de una función `@arena`: ahí una pila `_Thread_local` de regiones (en soso, una global) sirve el heap del host desde páginas de 64 KiB (`mmap`/`munmap` en linux, `malloc`/`free` en soso). `sal_arena_enter` / `sal_arena_leave` abren y descartan la región; `sal_xfree` de un puntero de cualquier región activa del hilo no hace nada; crecer un bloque de página reserva otro y abandona el viejo, sin `realloc` de libc. `sal_arena_alloc_in(dst, n)` reserva en la región que contiene `dst`, o con el `malloc` del sistema si no está en ninguna: por ahí pasan la copia de un `keep` (`sal_arena_export`) y el crecimiento de ese contenedor. Con `--instrument` la sonda cuenta la página, no cada objeto, y `leave` empareja ese `free`. `sal_region_begin` / `sal_region_end` siguen siendo el bump de la región fusionada de tensores (`peak_bytes`), no este asignador. Heaps distintos para `cpu`, `gpu` y `tpu`, aunque la máquina no tenga GPU ni TPU: `sal run` ejecuta el bloque en el heap de ese actor. gpu y tpu no entran en las páginas. Con `sal build --device gpu` y `nvcc` disponible, el enlace define `SAL_USE_CUDA`: reservas y `to gpu` usan memoria de dispositivo, y `sal_matmul_f32` en el heap gpu llama al núcleo CUDA (`runtime/gpu_matmul.cu`). Sin CUDA, el mismo contrato se emula en host. **`sal --sanitize build|run`** (o `SAL_SANITIZE=1`) compila el IR y el runtime C y enlaza con `-fsanitize=address,undefined` (LSan incluido), en `-O0`, con objetos cacheados aparte (`module.asan.o`); con `--target soso` es `E_TARGET`. Es independiente de `--instrument`: ASan ve fugas, doble free, uso tras liberar y desbordes aunque no pasen por `sal_xmalloc`. Límites: las páginas de `@arena` salen de `mmap` y ASan no las ve (sí las `malloc`); gpu y tpu son emulación en host. `tests/memory_asan.rs` barre con él `tests/lang/{drops,ownership,collections,structs,enums,strings,methods,generics}` y `tests/memory/`; `# asan-xfail: motivo` marca una fuga conocida.

`on gpu kernel i, j in …` baja a `KernelGrid` en la IR (bucles sobre la forma estática) y a `sal_on_enter` en dispositivo. `@shared` en el fuente sigue fuera del arranque; el mosaico gpu usa shared internamente. Hace falta reserva y liberación por lugar, `String`, `List[T]` con índice comprobado (`T` ∈ Int, Float, Bool, String o struct describible en descriptor `S(…)`), `Dict[K, V]` (`sal_dict_*`, distinto de `sal_map_*` del autohospedaje), `Tensor` con elemento y forma, `print` y `panic`. Struct describible: `sal_struct_new` / `desc_copy` / `sal_struct_free` / `sal_struct_free_box` (solo el bloque de una copia escalarizada).

`load` lee un `.salt`: magic `SALT`, versión u16, elem u8, rank u8, dims u64 little-endian, payload little-endian. El tensor resultante está en `cpu`. Los dos compiladores llaman a `sal_load_f32_checked`: si el fichero no existe o no se puede leer como `.salt`, es un pánico (`sal panic: cannot load tensor from <ruta>`, sale con 1), nunca un tensor nulo. `sal_load_f32` sigue devolviendo `NULL` para quien lo llame desde C (`tests/exec_salt.rs`). El `to` posterior es el único traslado. Si la reserva del dispositivo falla, el proceso termina con `E_OOM`: lugar, bytes pedidos y span de la región, en el JSON de diagnósticos.

`any[P]`: caja heap `sal_any_box` / `sal_any_free`; vtable por par `(T, P)` generada en el módulo; `sal_any_call` para métodos del protocolo. IR: `AnyBox`, `AnyCall`.

`runtime/kernels.c`: `matmul` en mosaico, filas empaquetadas. `F32` y `F16` tienen núcleo; `F16`, `BF16` e `I8` acumulan en `F32`. En cpu, `BF16` e `I8` se ensanchan hacia ese núcleo. La IR conserva el tipo estrecho y el nombre de la primitiva para que otro controlador sustituya el cuerpo. `softmax` suma en orden de índice. Operadores columnares F32 (`sal_tensor_bin_f32`, `sal_tensor_cmp_f32`) usan bucle host también en heap gpu hasta un `.cu` dedicado. `where` en el bootstrap expande `sal_where` a `sal_mask_select_count`, `sal_tensor_select_*` y `sal_list_select`.

## LLVM

`src/llvm.rs` emite IR textual de cada función sal, no solo de `main`. Cada instrucción de la tabla de arriba tiene bajada. Las primitivas se declaran y se llaman (`sal_matmul_f32`, `sal_place_copy`, `sal_load`, …). Un bloque de dispositivo baja a la IR de dispositivo y al runtime del heap de ese actor.

`sal build` enlaza con Clang `-O0`. `sal build --release` usa `-O3`. Sin `--instrument` no hay sondas ni contabilidad de reservas.

## Hecho cuando

- La IR de `relu(matmul(x, w))` es una región, sin intermedio materializado, y `peak_bytes` sale de elemento por forma.
- Un producto 2×2 conocido coincide con el número esperado, también leído de un `.salt`.
- `sal run examples/hello.sal` termina en 42.
- El LLVM del forward declara y llama a `sal_matmul_f32` y, si hay `to`, a `sal_place_copy`.
- `cargo test --test drops` incluye `sal_struct_free_box` tras `read_n(o)` con `o = list_get` de struct, y `sal_list_free` de un campo `List[P]` en cola.
- `SAL_INSTRUMENT_STRICT=1` sobre `tests/lang/drops/drop_list_struct_box_pass_fn.sal`: sin `LEAK` ni `struct_copy` residual.


**Enums.** Una variante sin campos es su tag (`i64` < 4096); una con campos es un puntero a una caja `[tag|kinds<<8, n, campo0, …]` (`sal_enum_new/set/tag/field`). IR: `EnumMake { payload: Vec, owned }` (`owned`: dos bits por campo, 1 `String` fresco, 2 caja de enum fresca), `EnumTag`, `EnumField`, `Switch` sobre `EnumTag`. `dict_get` construye un `Some` del preludio; si el valor del dict es `String`, la cabecera del `Some` marca dueño del campo copiado. `list_set`, `list_pop`, `dict_has` y `dict_get` son builtins del runtime.

**Identidad del AST (autohospedaje).** `enum_box_id(e)` baja a `sal_enum_box_id` en C (`runtime/sal_runtime.c`): devuelve `Int`, solo lectura, la caja de una variante con campos, o `0` en unitarias o valores empaquetados en el tag. Las listas compartidas (`stmts`, argumentos de llamada, `elsifs`, `arms`) se deduplican por `list_box_id` en `tp_free*` (`selfhost/ast/tast.sal`). El bootstrap declara `sal_enum_box_id` en `src/llvm.rs` y la usa en `emit` igual que el resto de primitivas `sal_*`.

**Instrumentación.** `SAL_INSTRUMENT_STRICT=1` hace que `sal_instrument_shutdown` informe de todo bloque vivo (por defecto se omiten los sitios de cadenas y asas que el selfhost aún deja vivos a propósito). `tests/instrument_sal.rs` comprueba que un programa cuyos locals libera el compilador sale limpio en modo estricto (incl. cajas `struct_copy` de `list_get` + llamada escalarizada) y que una cadena pasada a una función de usuario se informa como `LEAK`.
