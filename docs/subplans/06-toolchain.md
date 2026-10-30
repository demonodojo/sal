# Subplan: herramientas

## Entrada

Un módulo que ya recorre los pases de [04-ir-runtime.md](04-ir-runtime.md) y, si se pide, el de [05-instrument.md](05-instrument.md).

## Salida

CLI `sal`, manifiesto `Sal.toml`, resolución de módulos y caché incremental en `target/`.

## Archivos

`src/main.rs`, `src/compile.rs`, `src/incremental.rs`, `src/lib.rs`, `Sal.toml`.

Tests: `tests/toolchain.rs`, `tests/run_hello.rs`.

## CLI

```text
sal check  <fichero>
sal build  <fichero>
sal run    <fichero>
sal fmt    <fichero>
sal emit   ast|typed|ir|llvm  <fichero>
sal fetch  [--update]
sal add    <nombre> --git URL [--version REQ]
```

Flags de `build` y `run`: `--release`, `--device cpu|gpu|tpu`, `--instrument`, `--instrument-out FICHERO`. `check` y los diagnósticos de cualquier comando aceptan `--error-format json`: una línea de stderr por diagnóstico, con `code`, `message`, `span` (`start`, `end` en bytes UTF-8; `line` y `col` 1-based) y `hint`.

`fmt` escribe la fuente canónica por stdout y sale 0 solo si el parseo ha ido bien. Si no parsea, no imprime una reescritura y sale distinto de cero.

`emit ast` imprime el JSON del esquema vigente (`schema_version` 2). `emit typed` añade tipos, lugares y `borrow`/`take`. `emit ir` imprime la región fusionada y `peak_bytes`. `emit llvm` imprime el IR textual que se le pasa a Clang.

`sal build --device gpu` o `--device tpu` elige el controlador compilado dentro de `sal`. Si el dispositivo o su toolchain no están, `E_DEVICE_MISSING`. `sal check` sigue aceptando el programa.

## Módulos

Un fichero, un módulo. `import` sigue la gramática: `IDENT` separado por `.`, o un `STRING` con ruta. La ruta se resuelve desde el directorio del fichero que importa, la raíz del proyecto, los `path` de `[dependencies]` en `Sal.toml`, los paquetes de `Sal.lock` en la caché global (`$SAL_PACKAGES` o `~/.sal/packages`), y la std bajo `SAL_HOME`. Dependencias locales por `path`; dependencias git con `version` semver en el manifiesto y `rev` fijado en `Sal.lock`. `sal fetch` clona o actualiza la caché y escribe el lock; `sal add` añade una entrada git al manifiesto y ejecuta `fetch`. `build`/`check` no usan red. Sin registro central ni macros.

El grafo de imports es el de reachability del módulo raíz. Un ciclo es `E_TYPE` con el span del `import`; no se sigue recursivamente sin límite. Ruta que no resuelve o choque de nombre entre módulos: `E_TYPE` en ese span.

`sal build` del fichero raíz compila cada módulo alcanzado a su `.o` (caché por módulo) y enlaza todos esos objetos con el runtime en un solo binario.

## Caché

`target/` guarda, por módulo, el objeto ya compilado y la región fusionada. La clave es el hash del AST tipado, la versión del compilador y las flags que cambian el binario: `--release`, `--instrument`, `--device`.

Un segundo `compile` de la misma clave reutiliza el `.o`. No lo regenera. Marca `cache_hit`. Tocar el módulo, o un módulo del que importa, invalida esa clave y las de quienes lo importan.

## Hecho cuando

- La segunda compilación del mismo módulo, sin tocar el fuente, tiene `cache_hit` y el `.o` conserva su mtime.
- Un `import` de otro fichero del proyecto resuelve y entra en el grafo.
- `sal fetch` con un repo git (incl. `file://` en tests) escribe `Sal.lock` y deja el paquete en la caché; un `import` al módulo del paquete compila sin red.
- `import "std/prelude.sal"` (o `std/soso/…`) resuelve desde un proyecto ajeno al árbol del compilador cuando `SAL_HOME` apunta a la instalación de sal.
- `--error-format json` emite un diagnóstico estable, el que consume [08-vscode.md](08-vscode.md).

## Consumidor

La extensión de VS Code usa `sal check --error-format json` y `sal fmt`. No se implementa aquí.
