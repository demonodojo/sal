---
name: sal-autoridad
description: >-
  Aplica la autoridad cerrada del lenguaje sal: SPEC.md manda sobre los
  subplanes y sobre el código, la gramática es recursiva por la izquierda, y
  el arranque rechaza lo reservado. Usar al cambiar gramática, tipos, memoria,
  efectos, lugares, tensores, frame, operadores columnares, where, códigos de
  error, o al proponer una construcción nueva del lenguaje.
---

# Autoridad del lenguaje sal

## Orden

1. [SPEC.md](../../../SPEC.md) es la gramática. Si un subplan diverge, manda la SPEC.
2. [docs/subplans/](../../../docs/subplans/) fija archivos, algoritmo y tests de cada pase. Si un pase de `src/` está incompleto, manda el subplan, no el stub.
3. No ampliar la superficie «porque el diseño maestro la menciona». Lo reservado es `E_PARSE` hasta su fase.

## Gramática

Recursiva por la izquierda en listas, operadores binarios y postfijos. Esa recursión es la asociatividad.

- `1 - 2 - 3` es `(1 - 2) - 3`. El árbol es `Binary { op: Sub, left: Binary(Sub, 1, 2), right: 3 }`.
- Igual `(a * b) * c`, `(a == b) == c`, `(a.b).c`, `(f(x))(y)`.
- Prefijos (`try`, `-`, `!`, `to`) y la lambda asocian a la derecha: `x => y => z` es `x => (y => z)`.
- `load[F32, 4, 4]("w.salt")` es `postfix` con `type_arg` (tipo, `INT` o `?`). `4` no es un tipo de nombre `"4"`.
- `if_expr` → `"if" expr suite elsif_list ("else" suite | ε)`, con `elsif_list` recursiva por la izquierda. Sin `else`, tipo `Unit` aunque haya `elsif`; con `else`, el tipo es el de la rama then. `elsif` y `else` se asocian al `if` interno que acaba de cerrar su suite. Dentro de `match`, `=>` es un brazo, no una lambda.
- Sangría con espacios. Un tabulador es `E_PARSE`. `#` no tiene producción.

Una reescritura recursiva por la derecha de `add`, `mul`, `cmp` o `postfix` no es esta gramática.

## Cerrado en el arranque

Compilación: `E_PARSE`, `E_TYPE`, `E_MOVED`, `E_EFFECT`, `E_PLACE`, `E_SHAPE`, `E_TENSOR_ELEM`, `E_DEVICE`, `E_DEVICE_MISSING`, `E_OOM`. `E_INTERNAL` solo para fallos del compilador, no para programas.

Instrumentación: `LEAK`, `OOB`, `USE_AFTER_FREE`, `DOUBLE_FREE`, `BAD_PLACE`, `NAN`, `INF`.

Fuera del arranque, y por tanto `E_PARSE` si aparecen: `parallel`, `@frozen`, `@stack`, `@layout`, `@resource`, `@shared`, `@arena`, `@no_heap`, `@copy`, diferenciación automática, registro de paquetes.

`Float` es f64 y no es elemento de tensor (`E_TENSOR_ELEM`). Elementos: `F32`, `F16`, `BF16`, `I8`.

## Tablas columnares (arranque)

- **Operadores sobre tensores**: si un operando de `+`, `-`, `*`, `/`, comparaciones o el prefijo `-` / `!` (máscara `I8`) es un `Tensor`, la operación es elemento a elemento. `String + String` sigue siendo concatenación. Mismo lugar (`E_PLACE`), mismo elemento o escalar difundido (`E_TENSOR_ELEM`), forma de rango 1 compatible (`E_SHAPE`). Comparaciones → `Tensor[I8, …] on lugar`.
- **`frame`**: item al nivel de `struct`. Columnas `F32` | `F16` | `BF16` | `I8` | `String`. Numéricas → `Tensor[Elem, ?] on p`; `String` → `List[String]` en `cpu`. Con columna `String`, el frame no admite `to gpu` / `to tpu` sobre el valor entero (`E_PLACE`); las numéricas pasan al dispositivo con `to` columna a columna. Construcción como struct: `Nombre(col1, …)`.
- **`where(tabla, mascara)`**: primitiva como `matmul`. Máscara `Tensor[I8, ?] on p` con el mismo largo de fila. Fuera de este corte pueden quedar groupby, join, CSV y la bajada runtime completa del filtro.

Dentro de `on`, cadenas de operadores columnares del mismo lugar forman región fusionada (`ColumnBin` en IR); ver [docs/subplans/04-ir-runtime.md](../../../docs/subplans/04-ir-runtime.md).

## Al cambiar el lenguaje

Tocar en el mismo cambio, y solo eso que el cambio exige:

1. La producción o la regla en `SPEC.md`.
2. El subplan del pase que la implementa.
3. El pase dueño (ver [sal-compilador](../sal-compilador/SKILL.md)).
4. Tests que comprueben el funcionamiento de lo añadido, en el mismo cambio. Sin ellos el cambio no está cerrado.

Cada construcción nueva (producción, token, tipo, efecto, lugar o bajada) lleva tests que la ejercitan y fallan si no hace lo que la SPEC dice. Un diagnóstico nuevo se afirma con su código `E_*` exacto. Un programa válido se afirma con el árbol exacto, el tipo, la IR o la ejecución, según el pase dueño. Si la superficie está en los dos compiladores, el test del arranque no sustituye al del escrito en sal: hay que cubrir los dos. Ver [sal-verificar](../sal-verificar/SKILL.md) y [sal-autohospedaje](../sal-autohospedaje/SKILL.md).

No inventar un código `E_*` ni un evento de instrumentación que no esté en la lista cerrada.
