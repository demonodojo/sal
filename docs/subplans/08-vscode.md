# Subplan: extensión de VS Code

## Entrada
CLI `sal` con `check --error-format json` y `fmt` a stdout. Códigos `E_*` y `span` de [src/diag.rs](../../src/diag.rs).

## Salida
Extensión en [editors/vscode/](../../editors/vscode/) que asocia `.sal`, colorea, sangra, muestra diagnósticos y formatea. El compilador sigue siendo la única gramática normativa.

## Archivos
`editors/vscode/package.json`, gramática TextMate, `language-configuration.json`, cliente de la CLI, formateador, tests del mapeo JSON → rango.

## Contrato
- Cada línea de stderr de `sal check --error-format json` es un `Diagnostic`: `code`, `message`, `span` (`start`, `end` en bytes UTF-8; `line` y `col` 1-based del inicio), `hint`.
- `sal fmt` imprime la fuente canónica por stdout. El buffer solo cambia si el proceso sale 0.
- Binario en `sal.compilerPath` (por defecto `sal` en el `PATH`).
- Buffer sin guardar: copia temporal en el mismo directorio, borrada al terminar, para no romper `import`.
- Ir a la definición de un nombre de función (Ctrl+clic, Cmd+clic o F12) busca el `fn` en el fichero y, si no está, sigue los `import` como el compilador: directorio del fuente, raíz del proyecto y `path` de `[dependencies]` en `Sal.toml`, en transitivo. No reimplementa el parser: reconoce líneas `fn` e `import`.

## Tests
Doble del proceso `sal` (sin invocar el compilador real ni la red): un `E_PARSE` cae en el span; el formateador no sustituye el buffer si el proceso no sale 0. La definición se prueba con un sistema de ficheros en memoria.

## Fuera de este subplan
Servidor de lenguaje propio, hover de tipos, autocompletado semántico, depurador. No se modifica `src/**` ni `runtime/**`.
