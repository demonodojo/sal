---
name: sal-stdlib
description: >-
  Biblioteca estándar de sal escrita en sal (std/io, std/http, std/json,
  std/text), la ABI de syscalls, los destinos linux/soso y las pruebas
  tests/sal con sal test. Usar al editar std/, runtime/soso/, std/io/abi.sal,
  tests/sal/, al usar syscall, o al añadir E/S, red, procesos o JSON en sal.
---

# Biblioteca estándar (`std/`)

Está escrita en sal y solo usa primitivas del compilador (`syscall`, `Bytes`, `String`, `List`). No hay C propio por módulo: el contrato con el sistema es la tabla de syscalls.

| Directorio | Contenido |
|------------|-----------|
| `std/prelude.sal` | `Option`, `Result`, firmas de primitivas. Hay que importarlo (`import "std/prelude.sal"`) |
| `std/io/` | `abi` (números de syscall), `io` (`Fd` + read/write/close), `fs` (`Path`), `cwd`, `env`, `net` (TCP/DNS), `pipe` (`Pipe`), `process` (`Wait`, spawn), `time`, `rand`, `util`. Wrappers `@deprecated` conservan borrow/take; `std/text/index_legacy.sal` conserva `str_index_*`. Tiene su propio `Sal.toml` (paquete `io`) |
| `std/http/` | `client` (petición, cabeceras, cuerpo, chunked), `sse` |
| `std/json/` | `jsonc`, `arena` (`JsonNode`), `build` |
| `std/text/` | `strutil`, `glob`, `pattern` |

Ver [sal-fuente](../sal-fuente/SKILL.md) para el formato del código.

## Destinos

`--target linux` (por defecto en un host Linux) o `--target soso` (kernel). `syscall(n, a0…a5) -> Int ! io` existe en los dos; en cualquiera otro es `E_TARGET`. En Linux lo resuelve el shim `runtime/soso/linux_host_syscall.c`, que implementa el **mismo contrato** que soso. Si añades una syscall: número en `std/io/abi.sal` (`nr_*()`), caso en el shim, envoltorio Sal, y un test.

Convenciones de la ABI (SPEC, sección `syscall`): `PIPE` devuelve `(escritura << 32) | lectura`; `WAIT` devuelve `(pid << 8) | (código & 0xff)`; en `SpawnIo`, un fd de stdio es `FD_INHERIT_TTY` (-1), un fd del padre, o `FD_CLOSED` (-12).

`sal selfhost` no está soportado con `--target soso`.

## Reglas de estilo de la biblioteca

- Los efectos se declaran (`! io, alloc`); una función que sólo calcula no los lleva.
- `Dict[K, V]` sólo admite `V ∈ Int | Float | Bool`. Para JSON se usa el patrón arena (subplan `10-json-arena.md`), no un `Dict[String, String]`.
- Para procesos en programas sal usa `std/io/process.sal`. `exec_capture` es una primitiva del bootstrap del compilador y sus tests (subplan `12-exec-capture.md`).
- Un proyecto externo ve `std/io` como `sal_modules/io/` (`import "io/io.sal"`) mediante `{ std = "io" }`; dentro del repo se importa `std/io/io.sal`. Ver [sal-paquetes](../sal-paquetes/SKILL.md).

## Pruebas (`tests/sal/`)

`sal test [dir]` (por defecto `tests/sal`) compila y ejecuta cada `test_*.sal`; exit 0 = pasa. El ayudante `tests/sal/support/assert.sal` (`assert_ne`, `assert_zero`, `assert_str_eq`, `assert_contains`) devuelve 0 si se cumple o un código distinto de cero que identifica la aserción. Cada paso lleva su propio código para saber cuál falló. `test_http_mock.sal` lee el puerto de `OC_MOCK_PORT` (7422 por defecto), así que necesita un servidor local escuchando ahí; compruébalo antes de dar por roto un fallo de conexión.

`tests/std_io_linux.rs` compila y ejecuta `examples/std_io_smoke.sal` para linux; es la comprobación mínima de que la ABI y el shim siguen alineados.

Referencias de diseño: `docs/subplans/11-opencode-parity.md` (checklist de syscalls y módulos para el cliente OpenCode en sal), `10-json-arena.md`, `12-exec-capture.md`.
