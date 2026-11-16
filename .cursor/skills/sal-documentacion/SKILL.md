---
name: sal-documentacion
description: >-
  Mantiene README.md (inglés) y LEEME.md (español) alineados con la SPEC, la CLI
  y los tests: afirmaciones verificables, ejemplos que compilan y ejecutan, y
  estado del arranque y del autohospedaje sin exagerar. Usar al editar cualquiera
  de las dos versiones, documentación de cara al usuario, tablas de comandos y
  flags, o ejemplos incluidos en la documentación.
---

# Documentación pública de sal

## Fuente de verdad

1. [SPEC.md](../../../SPEC.md) manda sobre el lenguaje: gramática, tipos, memoria, efectos, lugares, módulos y errores. Una afirmación de los README que no esté en la SPEC no se escribe.
2. La CLI real es `sal --help` y `sal <subcomando> --help`; los flags están en `src/main.rs`. No documentes un flag o subcomando de memoria.
3. El estado del autohospedaje está en [docs/subplans/07-selfhost.md](../../../docs/subplans/07-selfhost.md). El selfhost valida el lenguaje; no lo describas como equivalente al arranque en lo que el subplan lista como pendiente.
4. Un test con `# xfail:` o `# selfhost-xfail:` en `tests/lang/` es un hueco conocido: no lo presentes como característica soportada. Compruébalo antes de afirmar algo con `grep -rl xfail tests/lang`.

Si la SPEC y el código divergen, la documentación no elige un lado: se avisa al usuario y se cierra la divergencia en el mismo cambio.

## Reglas de contenido

- `README.md` va en inglés y `LEEME.md` es su versión en español. Los dos son el mismo documento: misma estructura, mismas secciones, mismas tablas y los mismos ejemplos; solo cambia el idioma. El enlace cruzado (`[Español](LEEME.md)` / `[English](README.md)`) va en la portada de ambos.
- Tono técnico y directo, en los dos idiomas. Sin superlativos ni promesas de rendimiento que no tengan una medición detrás.
- Cada afirmación de comportamiento debe poder señalarse en la SPEC o comprobarse con un test. «Más rápido», «completo» o «igual que Rust» solo con matices y evidencia.
- No inventes códigos `E_*`, eventos de instrumentación, atributos ni builtins. Las listas cerradas están en la SPEC y en [sal-autoridad](../sal-autoridad/SKILL.md).
- Los ejemplos son programas sal reales: indentación de espacios, efectos declarados, `&self` donde toca y `main` con el tipo de retorno correcto.
- Mantén la estructura de los dos documentos: portada, ejemplo ejecutable, características con código, instalación, uso, estructura y estado. Los enlaces a `SPEC.md`, subplanes y tests tienen que existir.
- Al tocar la tabla de flags, sincronízala con `sal --help`; al tocar la tabla de errores, con la lista cerrada de la SPEC.

## Verificar los ejemplos

No basta con que el snippet «parezca» válido: compílalo y, si es un ejemplo de ejecución, ejecútalo. Comprueba las dos versiones: el código traducido cambia comentarios y literales visibles, pero no la semántica.

```bash
# desde la raíz del proyecto, con el binario ya construido
target/release/sal check tmp_doc_snippet.sal
target/release/sal run tmp_doc_snippet.sal

# o con el binario de desarrollo
cargo run -- check tmp_doc_snippet.sal
cargo run -- run tmp_doc_snippet.sal
```

- Escribe el snippet como un fichero temporal **dentro del proyecto** (los `import` se resuelven desde la raíz y desde el directorio del fichero), ejecútalo y bórralo. No lo dejes en el árbol ni lo commitees.
- Si el snippet usa `print`, `read_file`, listas o `f"…"`, declara `! io` / `! alloc` como exige la comprobación de efectos.
- Si el snippet es un fragmento sin `main` (una firma, un `struct`, un brazo de `match`), comprueba al menos que encaja en un programa mínimo compilable; no publiques sintaxis que el parser rechaza.
- Un ejemplo de dispositivo (`gpu`/`tpu`) se compila con `sal check`; ejecutarlo exige el dispositivo o su emulación. Distingue lo comprobado de lo no ejecutado.
- Un snippet de imports o de sintaxis puede ser ilustrativo y no compilar aislado; comprueba al menos la parte ejecutable o deja claro que es un fragmento, y no lo cuentes como ejemplo verificado.
- `standard check` solo se aplica a ficheros `.sal`; no pasa nada por un bloque Markdown. Si el snippet se mueve a `examples/` o `tests/lang/`, entonces sí pasa por `sal fmt` y `standard check`.

## Mantener README.md y LEEME.md

- Toda edición de contenido se hace en las dos versiones en el mismo cambio. No dejes una sección nueva solo en inglés ni solo en español.
- El código de los snippets es el mismo en ambos, salvo comentarios y literales visibles traducidos; las firmas, tipos, nombres de API y estructura tienen que coincidir. Si traduces un literal, vuelve a compilar y ejecutar.
- El ejemplo completo de la portada debe seguir compilando y ejecutando. Si cambia la salida, actualiza el comentario y el valor esperado a mano en los dos.
- Las tablas de tipos, atributos, errores y flags se actualizan en el mismo cambio que la SPEC o la CLI, no en un cambio posterior, y en las dos versiones.
- Al añadir una característica nueva al lenguaje, revisa si los README la mencionan o si su sección se queda corta; no añadas una sección de marketing por cada pase.
- No conviertas los README en una copia de la SPEC: la SPEC es la referencia completa y los README la introducción con los ejemplos canónicos.
- Enlaza a `SPEC.md` y a los subplanes en lugar de duplicar detalles que cambian a menudo.

## Hecho

Cada snippet añadido o modificado compila con `sal check` en las dos versiones; los ejemplos ejecutables salen con el resultado que los README dicen. `README.md` y `LEEME.md` tienen la misma estructura, tablas y ejemplos, y el enlace cruzado funciona. Las tablas y listas citadas existen y coinciden con la SPEC, la CLI y los tests. El cambio solo toca documentación (y, si procede, el snippet de `examples/` que la respalda), sin arrastrar cambios de código ajenos a la tarea.
