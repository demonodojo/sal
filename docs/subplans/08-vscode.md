# Subplan: extensión de VS Code

## Entrada
CLI `sal` con `check --error-format json` y `fmt` a stdout. CLI `standard` con `check` (avisos `path:line:col: Style/…: mensaje` en stderr) y `fix` (reescribe el fichero). Códigos `E_*` y `span` de [src/diag.rs](../../src/diag.rs).

## Salida
Extensión en [editors/vscode/](../../editors/vscode/) que asocia `.sal`, colorea, sangra, muestra diagnósticos y formatea. El compilador sigue siendo la única gramática normativa.

## Archivos
`editors/vscode/package.json`, gramática TextMate, `language-configuration.json`, cliente de la CLI, formateador, tests del mapeo JSON → rango.

## Contrato
- Cada línea de stderr de `sal check --error-format json` con error es un `Diagnostic`: `code`, `message`, `span` (`start`, `end` en bytes UTF-8; `line` y `col` 1-based del inicio), `hint`. Los avisos `W_DEPRECATED` (`@deprecated`) salen en humano (`warning[W_DEPRECATED]: … (path:line:col)`) aunque `check` termine en 0; la extensión los parsea y los muestra como warning.
- Cada línea `path:line:col: Style/…: mensaje` de `standard check` es un aviso. Con `--error-format json`, cada línea es un objeto con `code`, `message`, `span` (`start`/`end` en bytes) y `fix` (el reemplazo, o `null`). La extensión ofrece ese `fix` como quick fix. Un binario `standard` ausente no borra los `E_*`.
- `sal fmt` imprime la fuente canónica por stdout. El buffer solo cambia si el proceso sale 0.
- `SAL: Standard Fix` ejecuta `standard fix` sobre una copia temporal y sustituye el buffer con ese texto si el proceso sale 0 o 1. Si el editor de ese buffer tiene una selección no vacía, pasa `--start` y `--end` (bytes UTF-8, intervalo semiabierto) y `standard` solo aplica los arreglos cuyo span solapa ese intervalo; el resto del texto no cambia. Sin selección, reescribe el buffer entero. No escribe el fichero guardado. El comando está en el menú contextual del editor, de la pestaña y del explorador cuando el recurso es un `.sal`. En el explorador y en la pestaña el argumento es ese fichero; en el editor y en la paleta, el buffer activo.
- Binario del compilador en `sal.compilerPath`. Binario del linter en `sal.standardPath`. Sin ruta explícita: `target/release` o `target/debug` del workspace, luego `bin/<plataforma>/` empaquetado en la extensión (misma versión que el release de sal), luego `PATH`.
- Buffer sin guardar: copia temporal en el mismo directorio, borrada al terminar, para no romper `import`.
- Ir a la definición (Ctrl+clic, Cmd+clic o F12) busca `fn`, `method`, `static`, `init` y una constante de módulo (`NOMBRE =` o `NOMBRE: Tipo =`, nombre `^[A-Z][A-Z0-9_]*$`) en el fichero (métodos también como `Tipo__nombre`) y, si no está, sigue los `import` como el compilador: directorio del fuente, raíz del proyecto y `path` de `[dependencies]` en `Sal.toml`, en transitivo. No reimplementa el parser: reconoce esas cabeceras e `import`.

## Tests
Doble de los procesos `sal` y `standard` (sin invocar el compilador real, el linter real ni la red): un `E_PARSE` cae en el span; un `Style/DupString` en JSON trae el reemplazo del quick fix; el formateador no sustituye el buffer si el proceso no sale 0; `standard fix` solo devuelve el texto reescrito si sale 0 o 1; una selección no vacía se traduce a bytes UTF-8 y `standard fix` recibe `--start` y `--end`. La definición se prueba con un sistema de ficheros en memoria. El filtro por rango (solo los arreglos que solapan el intervalo, también en un segundo pase) lo cubre `tests/standard.rs`.

## Fuera de este subplan
Servidor de lenguaje propio, hover de tipos, autocompletado semántico, depurador. No se modifica `src/**` ni `runtime/**`.
