# Semántica — siguientes pasos / huecos

Estado tras `03-semantics`: checks activos en `infer`, `ownership`, `effects`, `device`.
Suite: `CARGO_TARGET_DIR=/tmp/sal-semantics cargo test --offline --test semantics` → OK.

## Cubierto

| Código | Regla |
|--------|--------|
| `E_TENSOR_ELEM` | `Tensor[Float, …]` rechazado (parser + `infer` en `load`/tipos Named) |
| `E_SHAPE` | `matmul` con dims estáticas internas que no coinciden |
| `E_MOVED` | `String` / `Tensor` tras move (`=` o `to`); `Int`/`Float`/`Bool` no |
| `E_EFFECT` | `to gpu`/`on gpu` → `gpu`; `load` → `io`+`alloc`; `tensor[…]` → `alloc` |
| `E_PLACE` | tensor en `on` de otro lugar; `matmul` entre lugares distintos |
| `E_DEVICE` | `on tpu kernel` rechazado; `on gpu kernel` permitido |

## Huecos / limitaciones del parser o del pase

1. **Expresiones sueltas en el bloque**: solo la cola puede ser una expresión sin `=`. Hay que escribir `r = on gpu …` (u otra asignación) para tener stmts después de un `on`.
2. **Lambdas** (`x => …` en `map`): tipado del encoding `LAMBDA_CALLEE` hecho
   en infer/ownership/effects/device; `examples/gpu_roundtrip.sal` entra en la
   suite (`gpu_roundtrip_ok`). Variante `Expr::Lambda` aún pendiente (ver
   `02-frontend-siguiente.md`).
3. **Préstamo vs take en parámetros**: `ParamMode` existe; el pase de ownership aún no infiere `borrow`/`take` en firmas ni publica modos en el AST tipado más allá de lo que ya había.
4. **Escape / `@stack` / `@frozen` / `@resource`**: fuera de este subplan.
5. **`E_DEVICE_MISSING` / `E_OOM`**: runtime/toolchain, no semántica estática de este pase.
6. **Formas dinámicas `?`**: `matmul` solo falla en dims *estáticas* incompatibles; `?` se acepta (según SPEC).
7. **Compatibilidad de tipos en `infer`**: aún permisiva (`Unknown`, tensores entre sí); no es un HM completo.

## Notas de implementación

- `Assign` a un `Ident` nuevo se trata como binding (superficie sin `let`).
- `to` actualiza el `place` del tipo en `infer` y exige efecto de dispositivo en `effects`.
- `check_devices` lleva entorno de tipos/lugares; dentro de `on` exige que los tensores ya estén en ese lugar.
