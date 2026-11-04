---
name: sal-conformidad
description: >-
  Escribe y mantiene los tests de conformidad del lenguaje en tests/lang/<área>/*.sal,
  con el resultado esperado en la cabecera (exit, stdout, error, xfail,
  selfhost-xfail). Usar al añadir o cambiar un test de tests/lang, al marcar un
  hueco conocido, al arreglar uno, o al decidir cómo probar una construcción
  nueva del lenguaje.
---

# Conformidad del lenguaje (`tests/lang/`)

Cada `tests/lang/<área>/*.sal` es un programa completo cuyo resultado esperado está escrito **a mano** en comentarios al inicio del fichero. No se copia de la salida de ningún compilador: es lo que la [SPEC](../../../SPEC.md) dice que debe pasar.

## Cabecera

Solo se leen las líneas `#` iniciales (hasta la primera que no lo sea):

| Marca | Significado |
|-------|-------------|
| `# exit: N` | Construye, ejecuta y sale con `N` (byte bajo) |
| `# stdout: TEXTO` | Opcional. El stdout es exactamente `TEXTO` (sin el salto final) |
| `# error: CODIGO` | El compilador lo rechaza y el stderr contiene `CODIGO` (`E_TYPE`, `E_PARSE`, …) |
| `# xfail: MOTIVO` | Hueco conocido **del arranque**: tiene que seguir incumpliendo `exit`/`error`. Si pasa, el test falla hasta quitar la marca |
| `# selfhost-xfail: MOTIVO` | Hueco solo del selfhost: el arranque debe cumplirlo; el selfhost se salta el fichero |

Un `exit` o un `error` por fichero. Los diagnósticos usan la lista cerrada de la SPEC; no inventes códigos.

## Quién lo ejecuta

- `tests/lang.rs` → `cargo test --test lang`: compila cada fichero con `sal build` (arranque) y comprueba la cabecera. Un `xfail` que ya pasa es fallo.
- `tests/selfhost_ir.rs::selfhost_meets_language_conformance` → lo mismo con stage1 del selfhost; salta los ficheros con `xfail` o `selfhost-xfail`.

Los módulos auxiliares viven en `tests/lang/modules/mods/` (no llevan cabecera propia; los importa un test).

## Áreas (subdirectorios)

`tests/lang/` tiene un subdirectorio por área: `attributes`, `basics`, `collections`, `control_flow`, `drops`, `effects`, `enums`, `functions`, `generics`, `methods` (incluye `init`/`static`), `modules`, `numeric`, `ownership`, `strings`, `structs`, `syntax` (`E_PARSE`), `tensors`, `try`, `types`. Pon el test nuevo en el área de la construcción que prueba (los `err_*` van con su área, no aparte). Los nombres de fichero son únicos en todo el árbol (el binario de salida se llama como el fichero). Los tests de `modules/` importan `mods/…` relativo a su directorio. Los tres recorridos (`tests/lang.rs`, `selfhost_ir.rs`, `fmt_roundtrip.rs`) son recursivos y saltan `mods/`.

## Convenciones de nombre

Prefijos existentes: `err_*` (el programa debe fallar), `attr_*` (atributos), `tensor_*`, `struct_*`, `bytes_*`, `float_*`, `bitops`, etc. Un caso por comportamiento, con el nombre diciendo qué se afirma (`err_module_pattern_unknown_qualifier`).

## Cómo añadir una construcción

1. Un fichero que la use y afirme el resultado correcto (`exit`/`stdout`).
2. Un `err_*` por cada diagnóstico que la SPEC prometa, con su `E_*` exacto.
3. Si el selfhost aún no la soporta, `# selfhost-xfail: por qué` (con motivo concreto, no «pendiente»). Si el arranque tampoco, `# xfail:`.
4. Al arreglar el hueco, **quita la marca** en el mismo cambio.

Un test que solo pasa con el código viejo no cuenta; ver [sal-autoridad](../sal-autoridad/SKILL.md) y [sal-verificar](../sal-verificar/SKILL.md).

## Huecos conocidos hoy

Hay ficheros con `xfail` o `selfhost-xfail` (p. ej. `struct_value_alias`, `struct_enum_payload`, varios `tensor_*`). Antes de afirmar que algo está soportado, mira si su test lleva la marca. El subplan 07 dice que `tests/lang` ya no tiene huecos del arranque; eso quedó atrás: manda lo que hay en los ficheros (`grep -rl 'xfail' tests/lang`).
