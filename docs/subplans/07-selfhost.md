# Subplan: autohospedaje

## Entrada

El compilador de arranque en Rust, ya capaz de llevar un programa sal a un binario nativo según [04-ir-runtime.md](04-ir-runtime.md) y [06-toolchain.md](06-toolchain.md).

## Salida

`selfhost/` escrito en sal, `std/prelude.sal`, `corpus/` y `tests/selfhost_ir.rs`.

## Qué se hospeda

`selfhost/` es el mismo compilador, no un dialecto reducido ni un programa que devuelve 0. Lee fuente, construye el AST de la gramática recursiva por la izquierda, tipa, posee, baja a la misma IR y escribe el objeto. Usa `Result`, efectos `io` y `alloc`, y la caché por módulo. El parser de sal reconoce las mismas producciones que `src/parser.rs`: listas y operadores crecen por la izquierda; `1 - 2 - 3` tiene el mismo árbol en las dos implementaciones.

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
