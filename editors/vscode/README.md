# sal para VS Code

Extensión para editar `.sal`. El compilador sigue siendo quien acepta o rechaza el programa: la extensión colorea, sangra y muestra lo que devuelve la CLI.

## Requisitos

- Binarios del repo: `cargo build --bin sal --bin standard` en la raíz del proyecto.
- La extensión usa por defecto `target/debug/sal` y `target/debug/standard` del workspace abierto (así no pisa un `standard` viejo del PATH). Opcional: `sal.compilerPath` y `sal.standardPath`.

## Qué hace

- Asocia los ficheros `.sal` y muestra el icono de sal en el explorador.
- Colorea las palabras del lexer (`elsif`, `else`, `while`, `break`, `parallel` incluidas), `method` / `static` / `init`, atributos (`@frozen`, `@stack`, `@no_heap`, `@layout`, `@deprecated`), los tipos (`Tensor`, `F32`, `List`, `Bytes`, …), los comentarios `#`, las cadenas, `d"…"`, la interpolación `{nombre}` y el literal `tensor[…]`.
- Al pulsar Enter después de `fn`, `method`, `static`, `init`, `if`, `elsif`, `else`, `while`, `match`, `on`, `struct`, `enum`, o de una rama que termina en `=>`, sube un nivel de sangría.
- `SAL: Check` ejecuta `sal check --error-format json` y subraya cada `E_*` en su span (p. ej. `E_TYPE` cuando `+` exige `String` en ambos lados y falta `str_from_int` / `int_to_str`). También lee los avisos `warning[W_DEPRECATED]: …` del compilador (`@deprecated`). A la vez ejecuta `standard check --error-format json` y muestra cada `Style/*` como aviso (`StringPlus`, `PlusLiteral`, `DupString`, `Elsif`, …). Si el aviso trae un reemplazo, la bombilla ofrece ese quick fix.
- `SAL: Standard Fix` aplica `standard fix` al buffer (en una copia temporal) y deja el texto resultante en el editor. El fichero guardado no cambia hasta que se guarda el buffer.
- Format Document y `SAL: Format` aplican la salida de `sal fmt` solo si el proceso termina bien.
- Ir a la definición: con el cursor sobre el nombre de una función o método, Ctrl+clic (Cmd+clic en macOS) o F12 abre el `fn`, `method`, `static` o `init` correspondiente (los métodos se indexan también como `Tipo__nombre`). Busca en el fichero actual y, si no está, en los módulos de `import` (también a través de otros imports y de los `path` de `[dependencies]` en `Sal.toml`).

## Empaquetar

```bash
npm run compile
npx @vscode/vsce package --no-dependencies
```

Genera `sal-0.2.2.vsix` en este directorio (instalar con *Extensions: Install from VSIX…*).

Los tipos (`String`, `Int`, `Lex`, …) se pintan en verde solo en ficheros `.sal`; `fn` / `method` en violeta. Puedes cambiarlo en ajustes con `editor.tokenColorCustomizations` → `[sal]`.

## Probarla

Desde este directorio:

```bash
npm install
npm test
npm run compile
```

Abre `editors/vscode` en VS Code y lanza la configuración «Run sal extension».
