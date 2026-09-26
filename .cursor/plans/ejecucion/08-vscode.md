# Plan: Extensión de VS Code (08)

**Alcance fijo:** `editors/vscode/**`.

**No tocar:** `src/**`, `runtime/**`, `std/**`, `selfhost/**`, `corpus/**`, `examples/**` (la extensión los lee; no los cambia). El contrato ya está en la CLI: `sal check --error-format json` (un `Diagnostic` JSON por línea en stderr) y `sal fmt` (fuente canónica por stdout).

**Depende de:** B11. `check` y `fmt` tienen que existir; no hace falta GPU, TPU ni el autohospedaje.  
**Cierra:** B16. No bloquea B12–B15.

---

## T01 — Manifiesto

- **Archivo:** `editors/vscode/package.json`
- **Hacer:**
  - Lenguaje `sal`, extensión `.sal`, `onLanguage:sal`.
  - Contribuciones: gramática, configuración de lenguaje, formateador de documento, comandos `SAL: Check` y `SAL: Format`.
  - Setting `sal.compilerPath` (default `sal`).
- **Hecho:** VS Code (y un editor que cargue la misma API) ofrece el lenguaje al abrir un `.sal`.

## T02 — Gramática TextMate

- **Archivo:** `editors/vscode/syntaxes/sal.tmLanguage.json`
- **Hacer:** colorear las palabras del lexer: `fn`, `let`, `if`, `match`, `on`, `to`, `kernel`, `return`, `import`, `try`, `struct`, `enum`, `cpu`, `gpu`, `tpu`, `true`, `false`. Comentario `#` hasta fin de línea. Strings y números.
- **Hecho:** `examples/hello.sal` y un bloque `on` / `to gpu` quedan coloreados. La gramática no acepta ni rechaza programas: eso lo hace `sal check`.

## T03 — Indentación y comentarios

- **Archivo:** `editors/vscode/language-configuration.json`
- **Hacer:** comentario de línea `#`. Sin pares de llaves de bloque. Enter tras una cabecera (`fn`, `if`, `match`, `on`, `struct`, `enum`, y la rama que abre cuerpo) sube un nivel de sangría.
- **Hecho:** escribir un `fn` y pulsar Enter deja el cursor en el cuerpo, un nivel más adentro.

## T04 — Diagnósticos

- **Archivos:** cliente en `editors/vscode/src/`
- **Hacer:**
  - Ejecutar `sal check --error-format json` sobre el fichero.
  - Parsear stderr línea a línea. Campos: `code`, `message`, `span.start`, `span.end`, `span.line`, `span.col`, `hint`.
  - Rango del editor: `line`/`col` son 1-based y solo marcan el inicio; `start`/`end` son offsets de byte UTF-8 sobre el buffer. No tratar un code point como un byte.
  - Mostrar el `E_*` como código del diagnóstico y `hint` como información asociada.
  - Si el buffer no está guardado, copiar a un temporal en el mismo directorio y borrarlo en el `finally`.
  - Si el binario no se encuentra, un aviso de la extensión pide `sal.compilerPath`. Ese aviso no usa un código `E_*`.
- **Hecho:** un doble que escribe un `E_PARSE` en stderr coloca el subrayado en el span pedido.

## T05 — Formato

- **Archivo:** mismo cliente
- **Hacer:** Format Document ejecuta `sal fmt` y sustituye el buffer con stdout solo si el proceso sale 0. Si no parsea, el buffer queda igual.
- **Hecho:** un fuente ya canónico no cambia; un proceso con salida distinta de 0 no reescribe el editor.

## T06 — Comandos

- **Archivo:** mismo cliente
- **Hacer:** `SAL: Check` relanza T04. `SAL: Format` relanza T05.
- **Hecho:** ambos comandos aparecen en la paleta con un `.sal` abierto.

## T07 — Tests

- **Archivo:** `editors/vscode` tests del mapeo y del formateador
- **Hacer:** sustituir el proceso `sal` por un doble. Caso de diagnóstico (`E_PARSE` → rango). Caso de formato (stdout aplicado solo con exit 0). Sin red y sin lanzar el compilador real.
- **Hecho:** la suite de la extensión pasa con el doble.

## Fuera de este plan

- Servidor de lenguaje, hover, definición, autocompletado semántico, snippets que dupliquen el preludio, publicación en el marketplace.
- Depurador. El compilador ya pide DWARF fuera de release; enganchar gdb o lldb es otro encargo.
- Cambiar el JSON de diagnósticos o hacer que `fmt` lea stdin.

## Orden

1. T01–T03 (se puede abrir un `.sal` y editarlo).
2. T04–T06 cuando `sal check` y `sal fmt` respondan.
3. T07 cierra B16.
