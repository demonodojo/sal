---
name: sal-paquetes
description: >-
  Dependencias y resolución de imports de sal: Sal.toml, Sal.lock, sal fetch,
  sal install, sal add, dependencias path/git/github/std, caché global,
  sal_modules/ y símbolos modid__nombre. Usar al editar src/packages.rs,
  src/modules.rs, src/qualify.rs, selfhost/modules.sal, al tocar un manifiesto
  o lock, o ante errores «import not found».
---

# Paquetes y módulos

Autoridad: [SPEC.md](../../../SPEC.md) (sección Módulos) y [docs/subplans/06-toolchain.md](../../../docs/subplans/06-toolchain.md). Código: `src/packages.rs` (descarga, lock, install), `src/modules.rs` (grafo, resolución), `src/qualify.rs` (calificadores y alias). Espejo de solo lectura en `selfhost/modules.sal` y `selfhost/qualify.sal`.

## Manifiesto

```toml
[package]
name = "mi-app"
version = "0.1.0"

[dependencies]
local  = { path = "../local" }
json   = { git = "https://github.com/org/sal-json", version = "0.2" }
libx   = { github = "owner/repo", version = "0.1" }
io     = { std = "io" }
```

`version` es un requisito semver con semántica caret. Los tags son `vX.Y.Z`.

## Órdenes

| Orden | Hace |
|-------|------|
| `sal fetch [--update]` | Resuelve versiones, materializa en la caché y escribe `Sal.lock` (`name`, `version`, `git`, `rev`, y `sha256` en github) |
| `sal install [--update]` | `fetch` + enlaza (symlink) cada dependencia en `sal_modules/<nombre>`; un `sal_modules/<nombre>` que no sea symlink se rechaza |
| `sal add <nombre> --git URL [--version REQ]` | Edita el manifiesto y ejecuta `fetch` (versión por defecto `0.1`) |
| `sal check` / `sal build` | **Sin red.** Paquete del lock ausente en la caché → `E_TYPE` («import not found») |

## Fuentes de dependencia

- `path`: relativo a la raíz del proyecto.
- `git` + `version`: `git ls-remote --tags`, elige el tag más alto que cumple; clona en la caché.
- `github` + `version`: `GET /repos/owner/repo/releases` con `curl`; elige la release **publicada** (ni borrador ni prerelease) de mayor semver que cumple; descarga el tarball del tag. El lock guarda `git = "github:owner/repo"`, `rev` = tag y `sha256`, que se verifica si hay que volver a descargar.
- `std = "io"`: enlaza `std/io` del compilador.

Variables: `SAL_PACKAGES` (caché; por defecto `~/.sal/packages/<nombre>-<versión>/`), `GITHUB_TOKEN` (repos privados), `SAL_GITHUB_API` (raíz de la API; los tests usan `file://`), `SAL_STD` (sustituye `std/`).

## Orden de resolución de un `import`

Directorio del fichero que importa → raíz del proyecto → `sal_modules/` → `path` de `[dependencies]` → paquetes del `Sal.lock` en la caché. `import nombre/resto` busca `sal_modules/nombre/resto`, o la clave del manifiesto, o el `name` del lock. El texto del import no depende de dónde esté instalado sal.

## Calificadores y choques

- `import ruta as alias`: items solo como `alias.nombre` (llamadas, tipos, patrones).
- Sin `as`: se aplanan (transitivo) y el último segmento del path es calificador implícito de los items definidos en ese fichero.
- Nombre de item repetido entre módulos del grafo: símbolo de enlace `modid__nombre`, solo para los que chocan.
- `E_TYPE`: ruta inexistente, choque de nombres, `as` duplicado, alias igual a un item visible, `alias.x` inexistente, calificador implícito ambiguo, ciclo de imports.

## Tests

`tests/toolchain.rs`: `fetch_*`, `import_not_found_when_lock_package_missing_from_cache`, `sal_modules_import_resolves_inside_the_project`, `install_git_dep_*`, `install_path_dep_*`, `install_std_dep_*`, `install_rejects_*`, `install_github_release_dep_picks_highest_matching_release`. Fijan `SAL_PACKAGES` en un tempdir y usan repos `file://`: no tocan red ni `~/.sal`. Módulos: `tests/lang/mods/` y `tests/import_graph/`. Selfhost: `cargo test --test selfhost_ir selfhost_resolves_git`.

Un cambio de resolución se hace en `src/modules.rs` **y** `selfhost/modules.sal`; la descarga solo en Rust. Ver [sal-autohospedaje](../sal-autohospedaje/SKILL.md).
