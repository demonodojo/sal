---
name: sal-release
description: >-
  Publica una versión del compilador sal (repo demonodojo/sal): versión en
  Cargo.toml, pruebas, tag vX.Y.Z y GitHub Release. Usar al pedir release,
  publicar, tag o bump de versión de sal. No usar para opencode-sal.
---

# sal — publicar versión

Esto es el compilador **sal** (`demonodojo/sal`). No es opencode-sal: no edites ese repo, su `Sal.toml` ni sus tags.

No hay workflow de release. Publicar es commit en `main`, tag y `gh release create`. No hagas `cargo publish`: `sosomodel` es una dependencia `path` y este paquete no sale a crates.io.

## Versión canónica

La versión publicada es el tag git `vX.Y.Z` más alto (`git tag -l 'v*'`). `sal --version` imprime la de [`Cargo.toml`](../../../Cargo.toml) (`version` de clap). Las dos tienen que ser la misma `X.Y.Z` en el commit del tag.

Los tags `v0.1.0` y `v0.2.0` no subieron `Cargo.toml` (sigue en `0.1.0`). La siguiente versión es **posterior a `v0.2.0`**, no un bump de ese `0.1.0`.

`std/io/Sal.toml` es el paquete de E/S, no el compilador. No lo cambies en un release de sal.

## Flujo

### 1. Elegir versión

Desde el último tag, no desde el `0.1.0` atrasado de `Cargo.toml`:

- **Patch** (0.2.0 → 0.2.1): correcciones.
- **Minor** (0.2.0 → 0.3.0): lenguaje o compilador compatible.
- **Major**: ruptura de SPEC o de un binario ya publicado.

Si el usuario no da número, propone el bump según el diff desde el último tag y confirma antes de editar.

### 2. Fuentes

En el mismo commit, y solo si el usuario pide commit o publicar:

1. [`Cargo.toml`](../../../Cargo.toml) → `version = "X.Y.Z"`.
2. Regenera el lock y comprueba el paquete `sal`:

```bash
cargo update --offline -p sal --precise X.Y.Z
grep -A1 'name = "sal"' Cargo.lock
```

La línea `version` bajo `name = "sal"` en `Cargo.lock` tiene que ser `X.Y.Z`.

### 3. Verificar antes del tag

Árbol limpio de cosas que no sean este release. Desde la raíz de sal, con `../soso` presente (hace falta para compilar):

```bash
cargo build --locked --release --bin sal --bin standard
target/release/sal --version    # sal X.Y.Z
cargo test --locked
```

`cargo test --locked` es la puerta del tag. Si falla, no etiquetes: el arreglo sigue [sal-verificar](../sal-verificar/SKILL.md).

### 4. Commit

Solo si el usuario pide commit o publicar. Mensaje: `Release sal X.Y.Z`.

No incluyas cambios de `soso` ni de `opencode-sal`.

### 5. Tag y push

```bash
git tag -a "vX.Y.Z" -m "sal X.Y.Z"
git push origin HEAD
git push origin "vX.Y.Z"
```

El tag apunta al commit que ya tiene `Cargo.toml` en `X.Y.Z`.

### 6. GitHub Release

En x86_64:

```bash
cp target/release/sal "/tmp/sal-X.Y.Z-linux-x86_64"
cp target/release/standard "/tmp/standard-X.Y.Z-linux-x86_64"
gh release create "vX.Y.Z" --repo demonodojo/sal --title "sal X.Y.Z" --generate-notes \
  "/tmp/sal-X.Y.Z-linux-x86_64" "/tmp/standard-X.Y.Z-linux-x86_64"
```

En otra arquitectura, crea la release sin binarios y dilo.

Comprueba:

```bash
gh release view "vX.Y.Z" --repo demonodojo/sal
```

Tienen que estar el tag, el título `sal X.Y.Z` y, en x86_64, los dos binarios.

## Pin de soso

`soso/config/sal-source.lock` fija el commit de sal con el que soso compila. No lo toques al publicar sal. Solo si el usuario pide mover ese pin, en el repo `soso`, con el checkout de sal ya en el commit publicado:

```bash
python3 scripts/sal-source.py --update
```

Ese cambio es un commit aparte en `soso`.

## Errores frecuentes

| Síntoma | Causa |
|---------|--------|
| `sal --version` no es `sal X.Y.Z` | `Cargo.toml` no está en esa versión, o el binario es de antes del bump |
| `cargo test --locked` rechaza el lock | `Cargo.lock` no se regeneró tras cambiar `version` |
| La versión nueva es 0.1.1 | Se bumpeó el `Cargo.toml` atrasado; el último tag es `v0.2.0` |
| `cargo publish` | No aplica: dependencia `path` a `sosomodel` |
| Release de opencode-sal | Skill equivocada; esa publicación no es esta |
