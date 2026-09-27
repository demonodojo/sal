---
name: sal-autohospedaje
description: >-
  Mantiene el compilador escrito en sal, el preludio y el corpus de las tres
  cadenas con IR idéntica. Usar al editar selfhost/, std/prelude.sal, corpus/
  o tests/selfhost_ir.rs, o al hablar de stage1, stage2 o autohospedaje.
---

# Autohospedaje

`selfhost/` es el mismo compilador, no un dialecto ni un programa que devuelve 0. Lee fuente, construye el AST de la gramática recursiva por la izquierda, tipa, posee, baja a la misma IR y escribe el objeto.

## Cortes

Los módulos de `selfhost/` siguen los cortes del arranque: lexer, parser, tipos, ownership, efectos, lugares, IR, fusión, emisión. `1 - 2 - 3` tiene el mismo árbol que en `src/parser.rs`. En el parser, `parse_if` coincide con `src/parser.rs`: acumula `elsif` con `parse_elsif_arms` mientras `p_peek` sea `TK_ELSIF()`, y `else` solo si después es `TK_ELSE()`; si no, `ex_if(..., else_b = 0, arms)` y el resto del pipeline ya trata el bloque `0` como vacío. La bajada anida cada `elsif` como un `if` en la rama else.

Las primitivas de tensor no se reimplementan en sal. Se llaman y el runtime del arranque las resuelve.

`std/prelude.sal` define `Option`, `Result` y las firmas de `List`, `map`, `reduce`, `matmul`, `softmax`, `reshape`, `transpose`, `relu`, `load` y `print`. `load` se escribe `load[F32, 4, 4](path)`.

## Corpus

`corpus/` cubre, como mínimo: un movimiento legal, efectos declarados, un `to` explícito, un `forward` del que se mira `peak_bytes`, un `load` de `.salt` y un programa pensado para `--instrument`.

## Tres cadenas

```text
bootstrap Rust  → compila selfhost/  → stage1
stage1          → compila selfhost/  → stage2
bootstrap, stage1 y stage2 compilan cada fichero de corpus/
```

La IR normalizada de cada programa del corpus es idéntica en las tres. Normalizar quita nombres frescos que no sean estructurales y deja operaciones, lugares, `fused` y `peak_bytes`.

Stage1 sale de compilar `selfhost/` con el arranque. Stage2 sale de compilar `selfhost/` con stage1. Ninguno reimprime la IR del otro.

`selfhost/` compilado con `--instrument` termina sin `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN` ni `INF`. Compilar no exige GPU ni TPU.

## Cierre

`tests/selfhost_ir.rs` compara las tres IR y falla si una difiere. No marcar el criterio como cumplido con un stub.
