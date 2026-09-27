# sal para VS Code

Extensión para editar `.sal`. El compilador sigue siendo quien acepta o rechaza el programa: la extensión colorea, sangra y muestra lo que devuelve la CLI.

## Requisitos

- El binario `sal` en el `PATH`, o la ruta en `sal.compilerPath`.
- El binario `standard` en el `PATH`, o la ruta en `sal.standardPath`. Sin él, los `E_*` siguen apareciendo y la extensión avisa una vez.

## Qué hace

- Asocia los ficheros `.sal` y muestra el icono de sal en el explorador.
- Colorea las palabras del lexer (`elsif`, `else`, `while`, `parallel` incluidas), los tipos (`Tensor`, `F32`, `List`, …), los comentarios `#`, las cadenas, `d"…"`, la interpolación `{nombre}` y el literal `tensor[…]`.
- Al pulsar Enter después de `fn`, `if`, `elsif`, `else`, `while`, `match`, `on`, `struct`, `enum`, o de una rama que termina en `=>`, sube un nivel de sangría.
- `SAL: Check` ejecuta `sal check --error-format json` y subraya cada `E_*` en su span. A la vez ejecuta `standard check --error-format json` y muestra cada `Style/*` como aviso. Si el aviso trae un reemplazo, la bombilla ofrece ese quick fix.
- `SAL: Standard Fix` aplica `standard fix` al buffer (en una copia temporal) y deja el texto resultante en el editor. El fichero guardado no cambia hasta que se guarda el buffer.
- Format Document y `SAL: Format` aplican la salida de `sal fmt` solo si el proceso termina bien.
- Ir a la definición: con el cursor sobre el nombre de una función, Ctrl+clic (Cmd+clic en macOS) o F12 abre el `fn` donde está definida. Busca en el fichero actual y, si no está, en los módulos de `import` (también a través de otros imports y de los `path` de `[dependencies]` en `Sal.toml`).

## Probarla

Desde este directorio:

```bash
npm install
npm test
npm run compile
```

Abre `editors/vscode` en VS Code y lanza la configuración «Run sal extension».
