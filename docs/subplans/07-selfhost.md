# Subplan: autohospedaje

## Entrada

El compilador de arranque en Rust, ya capaz de llevar un programa sal a un binario nativo según [04-ir-runtime.md](04-ir-runtime.md) y [06-toolchain.md](06-toolchain.md).

## Salida

`selfhost/` escrito en sal, `std/prelude.sal`, `corpus/` y `tests/selfhost_ir.rs`.

## Qué se hospeda

`selfhost/` es el mismo compilador, no un dialecto reducido ni un programa que devuelve 0. Lee fuente, construye el AST de la gramática recursiva por la izquierda, tipa, posee, baja a la misma IR y escribe el objeto. Usa `Result`, efectos `io` y `alloc`, y la caché por módulo. El parser de sal reconoce las mismas producciones que `src/parser.rs`: listas y operadores crecen por la izquierda; `1 - 2 - 3` tiene el mismo árbol en las dos implementaciones. `d"…"` (`DSTRING`) se parsea como la misma llamada `strdup("…")` que en el arranque.

Los pases viven en módulos de `selfhost/` con los mismos cortes que el arranque: lexer, parser, tipos, ownership, efectos, lugares, IR, fusión, emisión. Las primitivas de tensor no se reimplementan en sal: se llaman, y el runtime del arranque las resuelve, igual que en cualquier otro programa.

`std/prelude.sal` define `Option`, `Result` y las firmas reales de `List`, `map`, `reduce`, `matmul`, `softmax`, `reshape`, `transpose`, `relu`, `load` y `print`. `load` se escribe `load[F32, 4, 4](path)`, que es la producción `postfix` de la SPEC. Los cuerpos de las primitivas de tensor no son un `t` devuelto tal cual: o delegan en la primitiva del runtime o no se declaran como funciones sal ordinarias que el usuario podría sombrear. El compilador las reconoce por nombre después de resolver el preludio.

## Corpus

`corpus/` tiene al menos:

- movimientos (`E_MOVED` no dispara; el programa es legal y usa un valor una sola vez)
- efectos declarados
- lugares, con `to` explícito
- un `forward` fusionado del que se mira `peak_bytes`
- un `load` de un `.salt`
- un programa pensado para compilarse con `--instrument`

## Tres cadenas

```text
bootstrap Rust  → compila selfhost/     → binario stage1
stage1          → compila selfhost/     → binario stage2
bootstrap, stage1 y stage2 compilan cada fichero de corpus/
```

La IR normalizada de cada programa del corpus es idéntica en las tres cadenas. Normalizar quita nombres frescos que no sean estructurales y deja operaciones, lugares, `fused` y `peak_bytes`.

`selfhost/` compilado con `--instrument` termina sin `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN` ni `INF`. Compilar no exige GPU ni TPU. Las pruebas del paquete, compiladas por ese binario, sí ejecutan `on gpu` y `on tpu` en el heap del runtime.

## Hecho cuando

`tests/selfhost_ir.rs` compara las tres IR del corpus y falla si una difiere. El binario de stage1 no es un stub que reimprime la IR del arranque: se obtiene compilando `selfhost/` con el arranque, y stage2 compilando `selfhost/` con stage1.

## Estado

El selfhost **no está completo**. Lo que hay hoy:

| Pase | Selfhost | Notas |
| --- | --- | --- |
| Lexer | `lexer.sal` (en sal, ya no `lex_src` de C) | Tokens de la SPEC incl. `let`, `match`, `try`, `=>`, bit ops. |
| Parser | `parser.sal` | Precedencias de la SPEC, `let`, lambda, `try`, `match`, `enum`, `frame`, parámetros de tipo, asignación a campos. Los tipos siguen siendo texto. |
| Efectos | `effects.sal` | Paridad con `src/effects.rs`. |
| Tipos | `infer.sal` | Solo escalares (`Int`/`Float`/`Bool`/`String`/`Unit`): variables, retornos, ramas de `if`, aridad y argumentos de funciones de usuario, operadores. `matmul` entre tensores con tipo declarado comprueba elemento, rango y dimensión interior (`E_SHAPE`); `transpose` (rango), `reshape` (volumen con literales) y `softmax` (eje literal) también, y `relu`/`silu`/`rope` conservan el tipo. Resto de tensores (`attention`, `qmatmul`, `reduce`, `rms_norm`), genéricos y builtins sin firma modelada son `Unknown`. |
| Ownership | `ownership.sal` | `E_MOVED`; tipos de valor simplificados. |
| Dispositivos | `device.sal` | `E_PLACE`/`E_DEVICE` con la misma lógica que `src/device.rs`; el parser reconoce `on <place> kernel`. |
| IR | `fuse.sal`, `tensorops.sal` | `if`/`while` con carries, struct/aggregate, drops de structs con `String`, drops de locales (`drops.sal`: `String`, `List`, `Dict`, `Bytes`, caja de enum), forma de `matmul`, `reshape`, `transpose`, `attention`, `rope`, `silu`, `tensor_append`, contenedores. |
| Backend | `emit.sal` emite IR de texto y **C** | No hay LLVM ni objeto propio. Dos caminos a C: desde el AST (escalares, strings, structs, bytes, `syscall`) y, para funciones con tensores, desde la IR (`cir_*`): `load`, `matmul`, `rope`, `silu`, `softmax`, `attention`, ops elemento a elemento, `to <place>`, literales. Los buffers de tensor llevan su longitud en una cabecera de 16 bytes y no se liberan. |

Los ejecutables del selfhost coinciden con los del arranque (código de salida y stdout) en los ejemplos y corpus que el arranque compila, salvo `examples/infer/tiny_*` (imprimen tiempos) y los que el arranque rechaza. Se mide con `selfhost_binaries_behave_like_bootstrap_binaries`.

Conformidad del lenguaje: `tests/lang/*.sal` lleva el resultado esperado escrito a mano (`# exit:`, `# stdout:`, `# error:`, `# xfail:` para huecos conocidos). `tests/lang.rs` lo comprueba con el arranque y `selfhost_meets_language_conformance` con el selfhost. `match` sobre enteros se baja en el emisor C del selfhost; enums, lambdas y `try` siguen pendientes. `selfhost_matches_bootstrap_on_every_repo_program` compara los dos compiladores sobre todos los `.sal` de `examples/`, `corpus/`, `tests/`, `std/` y `benches/` que el arranque compila. `tests/lang` ya no tiene huecos del arranque marcados con `# xfail:`. Enums con `match`, genéricos (tipos borrados a `Unknown`), lambdas (se inlinean en cada llamada), `try`/`Result`/`Option` (el preludio se añade si el programa los usa), `Float`, structs de varios campos, interpolación, propagación de efectos y `transpose` rank 2 estático (copia real vía `sal_transpose_f32`) funcionan en los dos compiladores. `# selfhost-xfail:` marca huecos solo del selfhost (`softmax` por filas, aridad de lambdas locales).

Paridad medida por `tests/selfhost_ir.rs::selfhost_ir_matches_bootstrap_on_parity_files`:
el IR del selfhost coincide con el del arranque en todos los `.sal` del repositorio que el arranque
lowera (incluido el propio `selfhost/`).

Pendiente, por orden:

1. `infer` completo: formas de `attention`, `qmatmul`, `reduce`, `rms_norm` y `E_TENSOR_ELEM`, genéricos, enums,
   builtins con firma. (Hecho: `matmul`, `transpose`, `reshape`, `softmax`; structs de varios campos en el emisor C, como puntero a
   array de slots `int64_t`.)
   Structs por valor: el arranque los reparte en registros antes de emitir LLVM (`src/scalarize.rs`) y no cambia la IR de texto, así que la paridad no se ve afectada. El selfhost los sigue emitiendo como puntero a un array de slots; `p.x = v` copia antes de escribir, para que un struct se comporte como valor. Huecos del selfhost: un struct dentro de una `List` y como payload de un enum (`tests/lang/struct_in_list.sal`, `struct_enum_payload.sal` llevan `# selfhost-xfail:`), y la copia en `a.b.c = v` anidado.
2. Los pases se aplican solo al compilar (`-o`), como `sal build`; `sal emit ir` no comprueba.
3. `match`, lambdas y `try` en el bajado a IR (hoy se parsean y se ignoran al bajar).
4. Tipos de verdad en el AST (hoy texto), diagnósticos con spans.
5. Backend LLVM / objeto, `--instrument`, kernels `on gpu kernel`.
6. Paquetes y `std` completos; `Result`/`Option` genéricos (el codegen del arranque falla con
   `match` de varias variantes: "duplicate case value in switch").

**Estado de la migración a tipos con dueño.** El selfhost ya compila enums con caja, structs de varios campos, `List[T]`/`Dict` y aplica el pase de drops tanto en el camino IR (`fuse.sal`) como en el AST→C (`emit.sal`, vía `drops_body`). Su propio AST sigue siendo asas `Int` (`vec_*`, `map_*`, `str_as_int`, ~280 `vec_new` y ~360 `str_as_int`); mientras sea así, el compilador autocompilado libera solo los locales simples y deja el resto vivo. Migrarlo exige reescribir parser, infer, ownership, effects, device, attrs, fuse, tensorops y emit sobre `enum Expr`/`enum Stmt`/`List[...]` y un modo de parámetro conocido por el pase de drops (hoy solo está en `src/ownership.rs`, no en `selfhost/ownership.sal`).
