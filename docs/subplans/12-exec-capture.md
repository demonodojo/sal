# `exec_capture` vs `std/io/process.sal`

| API | Dónde | Destino |
|-----|--------|---------|
| `exec_capture(bin, arg)` | Primitiva del compilador (`runtime/sal_runtime.c`) | Host nativo al compilar/ejecutar con el runtime del **compilador** (desarrollo, selfhost). |
| `spawn_capture_stdout`, `spawn_io_argv` | `std/io/process.sal` | Programas Sal enlazados para **`--target linux`** (shim soso) o **soso** (`syscall` / `SPAWN_IO`). |

Para OpenCode Sal en producción usar siempre `std/io/process.sal`. Reservar `exec_capture` al bootstrap del compilador y tests del propio repo `sal`.
