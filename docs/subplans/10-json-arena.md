# JSON en Sal: patrón arena (T85)

`Dict[K,V]` solo admite `V ∈ Int | Float | Bool`. Para árboles JSON usamos:

## JsonArena

- `struct JsonNode` con campos `kind: Int`, `str: String`, `num: Int`, `first_child: Int`, `next_sibling: Int`.
- `kind`: 0 null, 1 bool, 2 num, 3 str, 4 array, 5 object.
- Nodos en `List` materializada como buffers lineales (`sal_vec_*` en host; en soso, índices en `Int` sobre tabla `bytes`).
- Claves de objeto: nodo hijo tipo 3 (nombre) seguido del valor (convención OpenCode Sal).

Implementación de referencia: `opencode-sal/src/json_arena.sal`.

## Alternativa futura

Ampliar el compilador para `Dict[String, String]` y `List[struct]` nativos; hasta entonces el arena es el contrato.
