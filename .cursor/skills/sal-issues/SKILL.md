---
name: sal-issues
description: >-
  Atiende un issue abierto de demonodojo/sal de principio a fin: elige el más
  antiguo sin `working` ni `done`, lo marca, lo analiza y publica un plan, crea
  rama, escribe el test que falla, corrige, verifica y abre un pull request que
  cierra el issue con la etiqueta `done`. Usar al pedir «arregla el siguiente
  issue», «atiende los issues del repo» o automatizar el ciclo issue → PR.
---

# Atender un issue de sal

Esto es el compilador **sal** (`demonodojo/sal`). No es opencode-sal.

Invocar esta skill es la autorización para crear la rama, commitear, pushear, abrir el PR, etiquetar y cerrar el issue. Sin invocación explícita, no toques GitHub ni hagas commits (ver [AGENTS.md](../../../AGENTS.md), «Git»).

Un issue por ejecución salvo que el usuario pida varios.

## Etiquetas de estado

Dos, en inglés como el resto del repo: `working` y `done`. Si no existen, créalas una vez:

```bash
gh label create working --repo demonodojo/sal --color FBCA04 --description "En curso" 2>/dev/null || true
gh label create done    --repo demonodojo/sal --color 0E8A16 --description "Corregido en un PR" 2>/dev/null || true
```

`working` es el candado entre agentes: si otro lo puso, ese issue es de otro.

## 1. Elegir issue

```bash
gh issue list --repo demonodojo/sal --state open --limit 100 \
  --json number,title,labels,createdAt,url \
  --jq 'map(select(([.labels[].name] | index("working") | not) and ([.labels[].name] | index("done") | not)))
        | sort_by(.number) | .[0]'
```

- Sin resultado: dilo y para. No inventes trabajo ni abras issues.
- Descarta también el que ya tenga un PR abierto (`gh pr list --repo demonodojo/sal --state open --search "<número>"`) o que pida una decisión de producto: pasa al siguiente y anota por qué.
- El orden es por número, es decir, el más antiguo primero.

## 2. Marcarlo y analizarlo

```bash
gh issue edit <N> --repo demonodojo/sal --add-label working
gh issue view <N> --repo demonodojo/sal --comments
```

Analiza antes de tocar código:

1. **Reproduce** el fallo con un programa mínimo (`cargo run -- check|fmt|run|emit …`) o el comando que el issue describe. Guarda la salida exacta: es la línea base y va al PR.
2. **Localiza el dueño** según la tabla de [AGENTS.md](../../../AGENTS.md): `src/<pase>.rs`, `runtime/`, `standard/`, `std/`, `selfhost/` o el linter. Lee [SPEC.md](../../../SPEC.md) y el subplan de [docs/subplans/](../../../docs/subplans/) del pase antes de decidir el arreglo.
3. Respeta la autoridad: si SPEC y código divergen, el arreglo cierra la divergencia en el mismo cambio y se avisa al usuario. No amplíes la superficie del lenguaje, no inventes códigos `E_*` ni eventos de instrumentación, y no añadas lógica de memoria nueva a `check_ownership`/`insert_drops` (pases heredados, ver subplan 13).
4. Carga la skill del área antes de editar: [sal-compilador](../sal-compilador/SKILL.md) (`src/`, `runtime/`), [sal-memoria](../sal-memoria/SKILL.md) o [sal-memoria-asan](../sal-memoria-asan/SKILL.md), [sal-conformidad](../sal-conformidad/SKILL.md), [sal-standard](../sal-standard/SKILL.md), [sal-fuente](../sal-fuente/SKILL.md), [sal-stdlib](../sal-stdlib/SKILL.md), [sal-autohospedaje](../sal-autohospedaje/SKILL.md), [sal-paquetes](../sal-paquetes/SKILL.md).

**Plan** (pasos, ficheros dueños, tests concretos). Publícalo como comentario del issue para dejar rastro:

```bash
gh issue comment <N> --repo demonodojo/sal --body-file /tmp/plan-<N>.md
```

Si al analizar el issue resulta inviable, ambiguo o pide una decisión que no es tuya, comenta las preguntas, deja `working` puesto y para sin PR.

## 3. Rama

Antes de crear nada, `git status --short`: no arrastres cambios ajenos (hay trabajo en curso en `selfhost/`).

```bash
git fetch origin
git switch -c fix/<N>-<slug-corto> origin/main
```

`<slug-corto>` en kebab-case a partir del título del issue.

## 4. Test que falla primero

El test es parte del arreglo, no un extra. Según el área:

| Qué falla | Dónde va el test |
|-----------|------------------|
| Comportamiento del lenguaje, diagnóstico, `fmt` | `tests/lang/<área>/*.sal` con la cabecera escrita **a mano** (`exit`/`stdout`/`error`) según [sal-conformidad](../sal-conformidad/SKILL.md) |
| Pase del compilador, runtime, CLI, IR | `tests/<área>.rs` (`frontend`, `semantics`, `drops`, `forward_ir`, `toolchain`, `standard`, …) |
| Regla de estilo | cop en `standard/cops/`, fixtures `_in/_out` y caso en `tests/standard.rs` |
| `std/` o preludio | `tests/sal/test_*.sal` o el test de área correspondiente |
| Fuga / doble free / uso tras liberar | programa en `tests/memory/` o `tests/lang/`, y `cargo test --test memory_asan -- --test-threads=1` |

Ejecútalo y **comprueba que falla por el motivo del issue**:

```bash
cargo run -- check tests/lang/<área>/<test>.sal   # o el comando del test
```

Si el test pasa sin el arreglo, no prueba nada: reescríbelo hasta que falle por lo que el issue describe. Si el área ya arrastraba un `xfail`/`asan-xfail` con ese hueco, **quita la marca en este mismo cambio**.

## 5. Arreglar

- Cambio mínimo en el fichero dueño del pase. Nada de refactors ni features no pedidas.
- Si el cambio es de lenguaje, toca en el mismo commit SPEC + subplan + pase dueño + tests. Si la construcción vive en los dos compiladores, añade también `selfhost/<pase>.sal` y su test; el del arranque no sustituye al del selfhost.
- Si el cambio es de memoria, la regla tiene que existir ya en la SPEC igual que en Rust; contrasta antes de implementar.
- `.sal` editado: `sal fmt` idempotente y `target/release/standard check` (o `cargo run --bin standard -- check`) sin avisos.

## 6. Verificar

```bash
cargo test --test <área>                 # el test del pase
scripts/test-area.sh <área>              # suite del área afectada
cargo test --test memory_asan -- --test-threads=1   # solo si se tocó memoria
cargo test --test lang                   # solo si se tocó comportamiento del lenguaje
```

Criterio de hecho: el test nuevo pasa, la salida del programa mínimo coincide con lo que el issue espera, y no rompe la suite del área. `standard` y `selfhost` van en serie (`-- --test-threads=1`).

Si no consigues el verde, **no abras el PR**: deja la rama, comenta en el issue qué falla y qué falta, mantén `working` y explica el bloqueo al usuario.

## 7. Commit y push

Mensaje corto y concreto, estilo del repo (`git log --oneline`): una o dos frases de por qué, no un inventario. Incluye el test y el arreglo juntos; no mezcles cambios ajenos.

```bash
git add <ficheros del issue>
git commit -m "<mensaje>"
git push -u origin fix/<N>-<slug-corto>
```

## 8. Pull request

```bash
gh pr create --repo demonodojo/sal --base main --head fix/<N>-<slug-corto> \
  --title "<título del issue>" \
  --body-file /tmp/pr-<N>.md
```

El cuerpo lleva: `Closes #<N>`, el fallo reproducido con su salida, la causa, el arreglo, los tests añadidos y la verificación ejecutada con su resultado. Guarda la URL:

```bash
gh pr view --repo demonodojo/sal --json url,number --jq '.url'
```

## 9. Cerrar el issue

```bash
gh issue comment <N> --repo demonodojo/sal --body "Corregido en <URL del PR>."
gh issue edit <N> --repo demonodojo/sal --add-label done --remove-label working
gh issue close <N> --repo demonodojo/sal --reason completed
gh issue view <N> --repo demonodojo/sal --json state,labels,url
```

El cierre manual deja el issue cerrado antes del merge (es lo pedido). `Closes #<N>` en el PR sigue siendo correcto y evita que quede huérfano si el issue se reabre.

## Errores frecuentes

| Síntoma | Causa / qué hacer |
|---------|-------------------|
| Dos agentes en el mismo issue | `working` se consulta justo antes de editar; si ya está, elige el siguiente |
| El test pasa sin el arreglo | El test no reproduce el issue; reescríbelo o replantea el análisis |
| El issue pide algo que la SPEC no contempla | No lo implementes: comenta la divergencia y para |
| `xfail` que ahora pasa | Quita la marca en el mismo cambio o `cargo test --test lang` falla |
| `cargo test` en paralelo falla en `selfhost_ir`/`bins`/`standard` | Es interferencia conocida: `-- --test-threads=1` |
| PR sin `Closes #N` | El issue queda sin vínculo; edítalo (`gh pr edit <PR> --body-file …`) |
| Se cerró el issue sin PR verde | Reabre (`gh issue reopen <N>`), quita `done`, deja `working` y explica el bloqueo |
