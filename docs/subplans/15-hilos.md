# Subplan: hilos y sincronización

La autoridad de la superficie es [SPEC.md](../../SPEC.md) («Hilos»). Este subplan fija archivos, algoritmo y tests. Si diverge de la SPEC, manda la SPEC.

Objetivo: lanzar trabajo en paralelo con `Thread.spawn(f, args…)` y coordinarlo con `Semaphore`, `Mutex[T]` y `Channel[T]` sin salir del modelo de memoria de Rust. El hilo tiene ámbito (`std::thread::scope`): presta `&T` de quien lo lanza y se une antes de que lo prestado muera. No hay clausuras ni `Arc`.

## Decisiones

- **`f` es una `fn` con nombre.** Las lambdas no son valores (subplan 14), así que el hilo no puede llevarse una. `Thread.spawn(f, a…)` se reescribe antes de inferir tipos a `thread_spawn(f(a…))`: la llamada interior es una llamada normal de `f`, y así los pases de tipos, movimientos, préstamos, efectos y liberación tratan los argumentos sin reglas nuevas. Solo la bajada sabe que `thread_spawn` no ejecuta la llamada.
- **Sin trampolín.** Toda `fn` de sal baja a `i64 @sal_u_f(i64…)`. `sal_thread_spawn(lista, @f, argc, a0…a7)` guarda la dirección y los argumentos, y el hilo llama con un despacho por número de argumentos. El resultado `R` va a una `List[R]` dueña creada con la glue de `R`, y así un `Thread` sin `join` suelta `R` al liberarse. Por eso los argumentos y `R` caben en un i64: un struct de varios campos es `E_TYPE` en este corte.
- **Préstamos.** `borrowck::ref_carrying` trata `Thread` como un tipo con lifetime, y `sources` recoge los préstamos de los argumentos de la llamada interior. Un `Thread` que su bloque no une en el nivel superior (fuera de ramas y bucles) se une en su glue al final del bloque: `keep_threads_to_drop` alarga sus préstamos hasta ahí.
- **`Mutex[T]` con `lock(x => …)`.** El método es el provisto de `protocol Lockable[T]` del preludio (`impl Mutex[T] Lockable[T]`), que usa los builtin `enter`/`leave`; la especialización de lambdas del subplan 14 hace el resto. El `&mut T` de `enter` es un préstamo exclusivo de `m` (`param_kind` `Mut` sin evento de mutación), así que no sobrevive a `leave`. Un `T` sin bloque propio vive en una celda (`sal_cell_*`), que es lo que recibe un parámetro `&mut T`; `ir::is_cell_handle` reconoce el resultado de `mutex_enter`. Cualquier otro `T` va en una `List[T]` de un elemento.
- **`Channel[T]`.** Cola `List[T]` dueña bajo un mutex y una variable de condición; `recv` construye el `Some` como `dict_get`.
- **soso.** `E_TARGET` en este corte: el kernel tiene `SYS_THREAD_SPAWN` y `SYS_FUTEX`, pero el runtime C no tiene TLS ni candados allí, y `SYS_EXIT` en un hilo solo mata ese hilo.

## Archivos (arranque)

| Pase | Archivo | Qué |
|------|---------|-----|
| Reescritura | `src/threads.rs` | `Thread.spawn(f, a…)` → `thread_spawn(f(a…))`, junto a `desugar_break` (`compile.rs`, `modules.rs`, `main.rs`) |
| Tipos | `src/infer.rs` (`infer_sync_builtin`), `src/methods.rs` | tipos `Thread`, `Semaphore`, `Mutex`, `Channel`; métodos y constructores |
| Protocolos | `src/protocols.rs` | `impl Mutex[T] P` con `enter`/`leave` builtin |
| Atributos | `src/attrs.rs` | `@arena` no lanza hilos |
| Efectos | `src/effects.rs` | `alloc` |
| Movimientos | `src/ownership.rs` | `join` mueve `t`, `send` mueve `v`, `Mutex.new` mueve `v` |
| Préstamos | `src/borrowck.rs` | `Thread` lleva préstamos; `enter` presta en exclusiva |
| Liberación | `src/drops.rs` | `thread_free`, `sem_free`, `mutex_free`, `chan_free`; resultado de `join` y de `recv` |
| Bajada | `src/ir.rs`, `src/llvm.rs` | `thread_spawn`, `chan_new`, `mutex_new`; declaraciones i64 |
| Runtime | `runtime/sal_runtime.c`, `runtime/instrument.c`, `src/compile.rs` | primitivas pthread; regiones y slab del lexer por hilo; candado en heaps de lugar e instrumentación; `-pthread` |
| Preludio | `std/prelude.sal` | `protocol Lockable[T]`, `impl Mutex[T] Lockable[T]` |

## Archivos (selfhost)

| Pase | Archivo | Qué |
|------|---------|-----|
| Reescritura | `selfhost/front/threads.sal`, `selfhost/front/parser.sal` (`desugar_spawn`) | la misma reescritura, tras `desugar_break` |
| Tipos | `selfhost/sem/infer.sal` (`mth_sync_builtin`, `infer_t_sync`, `infer_spawn_target`), `selfhost/ast/ttypes.sal` (`tt_sync_type`) | tipos, métodos, constructores y las restricciones del corte |
| Protocolos | `selfhost/sem/protocols.sal` (`check_mutex_builtin`) | `impl Mutex[T] P` |
| Lambdas | `selfhost/sem/lambda_specialize.sal` (`lambda_guess_sync_type`, `lambda_subst_bound`) | tipo de `Mutex.new(v)` para especializar `lock`; los argumentos ya ligados tipan los parámetros de la lambda |
| Atributos | `selfhost/sem/attrs.sal` (`at_spawns_block`) | `@arena` no lanza hilos |
| Efectos | `selfhost/sem/effects.sal` (`fx_sync_alloc`) | `alloc` |
| Movimientos | `selfhost/sem/ownership.sal` (`call_moves_arg`) | `join`, `send`, `Mutex.new` |
| Préstamos | `selfhost/sem/borrowck.sal` (`bk_bind_thread`, `bk_keep_threads`, `bk_enter_place`, `bk_recv_loan`) | `Thread` (tipo 4) lleva los préstamos; `enter` presta en exclusiva; el receptor de un builtin que muta (`mutates_container`) se presta `&mut` y el de una primitiva de hilos `&` |
| Liberación | `selfhost/ir/drops.sal` (`dr_sync_kind`, `dr_joined_kind`, `dr_note_spawns`) | glue de cada tipo; resultado de `join` y de `recv` |
| Bajada | `selfhost/ir/fuse.sal` (`lower_thread_spawn`, `lower_chan_new`, `lower_mutex_new`, `mutex_enter_cell`), `selfhost/emit/cir.sal`, `selfhost/emit/emit.sal` | la IR del arranque; la dirección de `f` en C |

## Tests

- `tests/lang/threads/*.sal`: ejecución (`spawn_join_sum`, `spawn_lends_list`, `spawn_moves_string`, `thread_drop_joins`, `thread_join_ends_loan`, `semaphore_*`, `mutex_*`, `channel_*`) y errores (`err_*`: `E_TYPE`, `E_BORROW`, `E_MOVED`, `E_EFFECT`). Los dos compiladores los cumplen sin `xfail` ni `selfhost-xfail`.
- `tests/selfhost_ir.rs` (`PARITY_FILES`): IR idéntica en los programas de `tests/lang/threads` salvo `channel_producer` (ver «Pendiente»).
- `tests/memory_asan.rs` incluye el área `threads`.
- `tests/runtime_str_threads.rs`: arnés TSan del runtime (`tests/support/sal_sync_threads.c`).
- `tests/soso_target.rs` (`soso_rejects_threads`): `E_TARGET`.

## Estado

| Parte | Arranque | Selfhost |
|-------|----------|----------|
| Runtime | Hecho | (comparte el runtime) |
| `Thread`, `Semaphore`, `Channel`, `Mutex` | Hecho | Hecho |

## Pendiente

- Selfhost: el brazo `None` de `match &r` sobre el `Option` de `recv` (y de `dict_get`) baja distinto que en el arranque; por eso `channel_producer` no está en `PARITY_FILES`.
- Structs de varios campos como argumento o resultado de un hilo, y como `T` de `Mutex`.
- `&mut T` como argumento de `Thread.spawn`.
- soso.
- `match ch.recv()` sin ligar el resultado (y `match r` que mueve el payload) fuga el `Some`, igual que `match d.get(k)`: es el hueco general de `match` sobre un temporal con dueño, no de los hilos. Los tests ligan el resultado y hacen `match &r`.
