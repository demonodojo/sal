# sal para VS Code

Extensión para editar `.sal`. El compilador sigue siendo quien acepta o rechaza el programa: la extensión colorea, sangra y muestra lo que devuelve la CLI.

## Requisitos

- Binarios del repo: `cargo build --release --bin sal --bin standard` en la raíz del proyecto.
- La extensión busca, en este orden: `sal.compilerPath` / `sal.standardPath`; `target/release` o `target/debug` del workspace; `bin/<plataforma>/` dentro de la extensión (sal **0.6.1** empaquetado); `sal` / `standard` en el `PATH`.
- Para refrescar los binarios del VSIX: `./scripts/copy-toolchain.sh` (desde `editors/vscode/`).

## Qué hace

- Asocia los ficheros `.sal` y muestra el icono de sal en el explorador.
- Colorea las palabras del lexer (`elsif`, `else`, `while`, `break`, `parallel` incluidas), `method` / `static` / `init` / `protocol` / `impl` / `mut` / `self`, las constantes de módulo (`NOMBRE` en mayúsculas), atributos (`@frozen`, `@stack`, `@no_heap`, `@arena`, `@layout`, `@deprecated`), los tipos (`Tensor`, `F32`, `List`, `Bytes`, `any`, …), los lifetimes (`'a`), los comentarios `#`, las cadenas, `d"…"`, la interpolación `#{nombre}` y el literal `tensor[…]`.
- Al pulsar Enter después de `fn`, `method`, `static`, `init`, `if`, `elsif`, `else`, `while`, `match`, `on`, `struct`, `enum`, o de una rama que termina en `=>`, sube un nivel de sangría.
- `SAL: Check` ejecuta `sal check --error-format json` y subraya cada `E_*` en su span (p. ej. `E_TYPE` cuando `+` exige `String` en ambos lados y falta `str_from_int` / `int_to_str`). También lee los avisos `warning[W_DEPRECATED]: …` del compilador (`@deprecated`). A la vez ejecuta `standard check --error-format json` y muestra cada `Style/*` como aviso (`ConstFn`, `StringPlus`, `PlusLiteral`, `DupString`, `Elsif`, …). Si el aviso trae un reemplazo, la bombilla ofrece ese quick fix.
- `SAL: Standard Fix` aplica `standard fix` al buffer (en una copia temporal) y deja el texto resultante en el editor. Si hay un trozo seleccionado, solo reescribe los arreglos de ese trozo; sin selección, el buffer entero. El fichero guardado no cambia hasta que se guarda el buffer. Está en el menú del botón derecho del editor, de la pestaña y del explorador sobre un `.sal`.
- Format Document y `SAL: Format` aplican la salida de `sal fmt` solo si el proceso termina bien.
- Ir a la definición: con el cursor sobre el nombre de una función, un método o una constante de módulo, Ctrl+clic (Cmd+clic en macOS) o F12 abre el `fn`, `method`, `static`, `init` o `NOMBRE =` correspondiente (los métodos se indexan también como `Tipo__nombre`). Busca en el fichero actual y, si no está, en los módulos de `import` (también a través de otros imports y de los `path` de `[dependencies]` en `Sal.toml`).

## Empaquetar

```bash
./scripts/copy-toolchain.sh   # opcional: sal y standard release en bin/
npm run package               # genera sal-0.6.1.vsix sin prompts
```

Genera `sal-0.6.1.vsix` en este directorio (instalar con *Extensions: Install from VSIX…* o `cursor --install-extension sal-0.6.1.vsix`). Ejecuta `./scripts/copy-toolchain.sh` antes si quieres incluir `sal` y `standard` release en el paquete.

Los tipos (`String`, `Int`, `Lex`, …) se pintan en verde solo en ficheros `.sal`; `fn` / `method` en violeta. Puedes cambiarlo en ajustes con `editor.tokenColorCustomizations` → `[sal]`.

## Probarla

Desde este directorio:

```bash
npm install
npm test
npm run compile
```

Abre `editors/vscode` en VS Code y lanza la configuración «Run sal extension».
