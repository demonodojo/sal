# Subplan: instrumentación

## Entrada

IR de [04-ir-runtime.md](04-ir-runtime.md) y las flags `--instrument` y `--instrument-out`.

## Salida

Sondas insertadas al bajar a LLVM, `runtime/instrument.c` y eventos JSON versionados.

## Archivos

`runtime/instrument.c`, prototipos en `runtime/sal_runtime.h`, inserción en `src/llvm.rs`, tests `tests/instrument_*.rs`.

### Qué se inserta

Solo con `--instrument`. Sin la flag el binario no lleva sondas, tampoco en `--release`. Con la flag, aunque vaya `--release`, hay sondas y se enlaza el runtime de depuración. Fuera de release se pide además `-g`.

Cada reserva, liberación, indexación de `List` o `Tensor`, y cada `place_copy` llama al runtime de instrumentación. El bloque guarda el span del fuente, el lugar, el tamaño y la pila de frames de sal. Alrededor del bloque hay una zona roja. La memoria liberada pasa a cuarentena y se envenena; no se devuelve al asignador mientras dure el proceso instrumentado.

Al salir, o al fallar, se escribe un evento por línea. En el terminal, texto. Con `--instrument-out`, el mismo evento en JSON. El esquema lleva versión.

| Evento | Cuándo |
|--------|--------|
| `LEAK` | el proceso termina con algún bloque vivo, en cualquier lugar |
| `OOB` | índice fuera de `List` o `Tensor`, o escritura en la zona roja |
| `USE_AFTER_FREE` | acceso a un bloque en cuarentena |
| `DOUBLE_FREE` | segunda liberación del mismo bloque |
| `BAD_PLACE` | un puntero se usa en el heap de otro actor |
| `NAN`, `INF` | una operación de tensor produce o recibe un no-número o un infinito; el evento lleva span, operación y lugar |

Cualquiera de esos eventos termina el proceso con código distinto de cero. Un programa que libera todo y no produce no-números termina en cero, con cero bloques vivos.

La traza, en el mismo JSON, registra `alloc`, `free`, `place_copy` y la entrada y salida de cada `on`. El pánico imprime esa pila de frames.

Fuera de este modo el runtime no recorre los resultados buscando `NAN` o `INF`.

## Tests

Por `sal run --instrument` sobre un `.sal`, sin red y sin un `.c` suelto como sujeto del test:

- una fuga emite `LEAK` con su span y sale distinto de cero
- un índice fuera de rango emite `OOB` y sale distinto de cero
- un tensor con un no-número emite `NAN` y sale distinto de cero
- un programa que libera todo sale en cero y no escribe eventos fatales

Los casos C de `tests/support/` pueden cubrir la cuarentena y la zona roja del runtime. No sustituyen la vía de la CLI.
