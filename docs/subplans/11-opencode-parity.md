# OpenCode 1.18.34 — checklist de paridad en Sal (T84 / T85 / T86)

Alcance acordado: `run`, subcomandos sin red, herramientas, sesiones JSON en disco. **Fuera:** TLS/HTTPS completo, SQLite nativo, TUI/serve, MCP/LSP, hilos, PTY.

Referencias: [10-json-arena.md](10-json-arena.md) (T85 arena), `opencode-sal/` (implementación de referencia).

---

## T84 — Fase A: runtime, ABI y `std/io` (bloqueante)

Objetivo: mismo contrato syscall en **soso** (guest) y en **`sal build --target linux`** (shim), más envoltorios Sal documentados para OpenCode.

### Tabla syscall (números soso-abi → Sal)

| Nº | Nombre soso | Uso OpenCode | `std/io/abi.sal` | `runtime/soso/linux_host_syscall.c` | Wrappers Sal |
|----|-------------|--------------|------------------|-------------------------------------|--------------|
| 21 | `PIPE` | tuberías explícitas antes de `spawn_io` | [ ] `nr_pipe()` | [ ] devolver par de fds al guest | [ ] `std/io/pipe.sal` (nuevo) |
| 23 | `CHDIR` | `--dir`, búsqueda hacia arriba | [ ] | [ ] | [ ] `std/io/cwd.sal` (nuevo) o `fs.sal` |
| 24 | `GETCWD` | `--dir`, rutas relativas | [ ] | [ ] | [ ] mismo módulo cwd |
| 72 | `CLOCK_GETTIME` (`CLOCK_REALTIME`) | `<env>`, `time.start`/`end`, timestamps ISO | [ ] + `clock_id` | [ ] | [ ] `std/io/time.sal` → `clock_realtime_*` |
| 78 | `GETRANDOM` | prefijos `ses_` / `msg_` / `prt_` | [ ] | [ ] | [ ] `std/io/util.sal` o `rand.sal` |
| 11 | `WAIT` | códigos de salida hijos | ok `nr_wait()` | ok `(pid<<8)\|exit` | [ ] helpers `wait_exit_code`, `wait_pid_status` en `process.sal` |
| 22 | `SPAWN_IO` | bash, git, subprocesos | ok | **parcial** (ver nota) | [ ] `spawn_argv`, cwd, fds no heredados |
| 6 | `STAT` | glob, mtime, tamaño, dirs | ok | ok (`SosoStat`) | [ ] `stat_size`, `stat_mtime`, `is_dir`, `mkdir_p` en `fs.sal` |
| 29 + 40 | `TCP_CONNECT`, `DNS_RESOLVE` | baseURL LAN | ok | ok | [ ] `tcp_connect_host`, parse `http://host:port` en `net.sal` |

Syscalls ya alineadas (solo mantener): read/write/open/close, getdents, mkdir, unlink, rename, sleep, uptime (monotónico), read_timeout, getenv, kill.

### Nota crítica: `SPAWN_IO` y tuberías en Linux

En `linux_host_syscall.c`, `soso_spawn_io` crea `pipe()` cuando stdin/stdout/stderr **no** son heredados, pero el padre **cierra el extremo de lectura** y solo devuelve el `pid`. Hoy no hay forma Sal de leer stdout/stderr del hijo tras `wait`.

Checklist shim + API:

- [ ] Definir contrato soso (kernel/SDK): ¿fds de lectura en campos extra de `SpawnIo`, segunda syscall, o siempre `PIPE` + pasar fd al spawn?
- [ ] Implementar el mismo contrato en `linux_host_syscall.c` (sin perder fds legibles en el padre).
- [ ] Exponer en `abi.sal` offsets faltantes de `SpawnIo` (`spawnio_off_cwd_*`, `spawnio_off_envp_*` ya existen en C, tamaño 112).
- [ ] `process.sal`: `spawn_capture(path, argv…)` → `{ pid, stdout_fd, stderr_fd }` o lectura a `String` con límite configurable.

Constantes sugeridas para spawn fds (documentar en `abi.sal` si no están):

- [ ] `spawn_fd_inherit()`, `spawn_fd_null()` (valores `SOSO_FD_*` del SDK).

### Ficheros a tocar (T84)

| Fichero | Tareas |
|---------|--------|
| `std/io/abi.sal` | Números 21, 23, 24, 72, 78; offsets stat/dirent/spawnio; `clock_realtime()` id |
| `runtime/soso/linux_host_syscall.c` | Casos syscall nuevos; arreglo spawn+pipes; paridad con soso-abi |
| `runtime/sal_runtime.c` | Solo si hace falta puente host (p. ej. `exec_capture` documentado para `--target linux`) |
| `std/io/time.sal` | `clock_gettime` real + helpers ms/nanos desde epoch |
| `std/io/cwd.sal` (nuevo) | `getcwd`, `chdir`, opcional `find_up(filename)` |
| `std/io/pipe.sal` (nuevo) | `pipe_create`, `pipe_close_read/write` |
| `std/io/process.sal` | `spawn_argv` genérico (count + tab argv); cwd en spawn; decode wait; integración pipes |
| `std/io/fs.sal` | `stat_*`, `mkdir_p`, listado no recursivo/recursivo acotado |
| `std/io/net.sal` | `sockaddr_from_ip_port`, `tcp_connect_host(host, port, timeout_ms)` vía `dns_resolve` |
| `std/io/env.sal` | Revisar si `<env>` necesita reloj de pared (depende de `time.sal`) |
| `SPEC.md` | Listar syscalls nuevos en la sección `syscall` (números estables) |

### Verificación (T84)

- [ ] Programa `.sal` bajo `examples/` o `corpus/` que: `getcwd` → `chdir` → `getcwd`, `clock_gettime` ≠ solo `uptime_ms`, `getrandom` 16 bytes, `pipe` + `spawn_io` + leer stdout de `/bin/echo`, `stat` dir/file, `dns_resolve` + `tcp_connect` no loopback.
- [ ] `cargo test --test run_hello` (o test dedicado `tests/std_io_linux.rs`) con `sal build --target linux` + ejecutar.
- [ ] `sal build --target soso` sin regresiones (`tests/soso_target.rs`).
- [ ] Tras cambios en `abi.sal`, revisar autohospedaje si algún pase importa io (skill `sal-autohospedaje`).

---

## T85 — Fase B: librerías (bloqueante para paridad funcional; repo `opencode-sal` o `std/`)

### B1 — JSON / JSONC (arena)

Documento principal: [10-json-arena.md](10-json-arena.md).

- [ ] Parse + serialize con **orden de claves estable** (paridad byte a byte con capturas OpenCode).
- [ ] JSONC (comentarios / trailing comma) si el upstream lo usa en config.
- [ ] Números grandes / fracciones: política `Int` vs `Float` + `f64_bits_to_str` documentada.
- [ ] Literales Sal: evitar `"` con `{` sin escapar; tests con `char_to_str` donde haga falta.
- [ ] Ubicación: mantener `opencode-sal/src/json_arena.sal` o promover a `std/json/` cuando estabilice.

### B2 — HTTP cliente + SSE

- [ ] Lectura sin tope fijo (~64 KiB hoy): bucle `read` / `read_timeout`.
- [ ] `Content-Length`, `Transfer-Encoding: chunked`.
- [ ] Timeouts por fase (connect, headers, body).
- [ ] Parser SSE: líneas `data:`, `[DONE]`, acumulación `tool_calls` por índice (deltas OpenAI).
- [ ] Solo **HTTP** en LAN (sin TLS en alcance).

### B3 — Regex / glob

- [ ] Motor en Sal puro **o** primitiva runtime (decisión explícita en issue).
- [ ] Casos OpenCode: grep herramienta, glob paths, permisos si aplica.

### B4 — Procesos de alto nivel

- [ ] `exec_capture`-like uniforme: `--target linux` (shim) y `soso`.
- [ ] Documentar diferencia host `exec_capture` (compilador) vs `std/io/process.sal`.

### Verificación (T85)

- [ ] Tests Sal con HTTP mock (socket loopback + respuestas grabadas); **sin red** en CI.
- [ ] Tests golden JSON ↔ bytes de fixtures OpenCode 1.18.34.
- [ ] Mockear subprocess: no depender de git/bash reales en CI si es frágil (regla del proyecto).

---

## T86 — Fase C: compilador y toolchain (recomendable, no bloqueante)

- [ ] **Tipos:** `Dict[String, String]` y/o `JsonValue` / `List[struct Message]` materializado (SPEC + semántica + IR + runtime).
- [ ] **CLI:** subcomando `sal test` (runner de `.sal` en proyecto, mencionado en planes de toolchain).
- [ ] **Calidad:** `standard check` / `sal fmt` en CI de opencode-sal (no funcionalidad core).
- [ ] Reducir dependencia del arena T85 cuando C esté cerrada.

### Verificación (T86)

- [ ] `cargo test` frontend/semantics según superficie tocada (`sal-compilador` skill).
- [ ] Migración opcional de un módulo opencode-sal del arena a tipos nativos (criterio de cierre).

---

## Orden de trabajo sugerido

1. **T84** syscalls + arreglo **spawn/pipe** + `process.sal` / `cwd` / `time` / `fs` / `net`.
2. **T85** JSON golden → HTTP+SSE → regex → polish procesos.
3. **T86** cuando el coste del arena supere el beneficio de velocidad de entrega.

## Issues GitHub (plantilla)

| Issue | Título sugerido | Etiquetas |
|-------|-----------------|-----------|
| T84 | OpenCode: ABI + std/io (pipe, cwd, clock, getrandom, spawn capture) | opencode, runtime, std-io |
| T85 | OpenCode: JSON/HTTP/SSE/regex (arena + cliente) | opencode, library |
| T86 | OpenCode: tipos JSON/chat + `sal test` | opencode, compiler, toolchain |

Copiar secciones **Checklist** de este documento en el cuerpo de cada issue.
