# sal para VS Code

Extensión para editar `.sal`. El compilador sigue siendo quien acepta o rechaza el programa: la extensión colorea, sangra y muestra lo que devuelve la CLI.

## Requisitos

- El binario `sal` en el `PATH`, o la ruta en `sal.compilerPath`.

## Qué hace

- Asocia los ficheros `.sal` y muestra el icono de sal en el explorador.
- Colorea las palabras del lexer, los comentarios `#` y los literales.
- Al pulsar Enter después de `fn`, `if`, `match`, `on`, `struct`, `enum`, o de una rama que termina en `=>`, sube un nivel de sangría.
- `SAL: Check` ejecuta `sal check --error-format json` y subraya cada `E_*` en su span.
- Format Document y `SAL: Format` aplican la salida de `sal fmt` solo si el proceso termina bien.

## Probarla

Desde este directorio:

```bash
npm install
npm test
npm run compile
```

Abre `editors/vscode` en VS Code y lanza la configuración «Run sal extension».
