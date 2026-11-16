# Paquetes de Sal para soso

Repositorio de paquetes de [soso](https://github.com/demonodojo/soso) servido
tal cual por `raw.githubusercontent.com`:

```text
https://raw.githubusercontent.com/demonodojo/sal/main/paquetes
```

`soso-pkg` (0.4.0 en adelante) lo consulta por defecto junto al de soso. El
paquete `sal` lleva el compilador nativo (`/bin/sal`), el backend LLVM y el SDK
(`/lib/sal/`); `lib/sal/origen.txt` dice de qué versión y commit salió.

- `indice.txt` — índice `SOSOPKG indice v1`.
- `sal/<versión>/paquete.txt` y `paquete.pack` — manifiesto y contenido.

No se edita a mano: se genera con `scripts/paquete-soso.sh [etiqueta]`, que
compila la etiqueta en un worktree limpio con la receta de soso
(`cargo xtask sal-paquete`). Ver el paso «Paquete para soso» de la skill
`sal-release`.
