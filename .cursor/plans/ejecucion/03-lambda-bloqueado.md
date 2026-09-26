# Tipado lambda — estado

## Bloqueo inicial (resuelto por el otro agente)

Al arrancar esta pasada, `cargo test` fallaba con:

```text
error[E0026]: variant `ast::Stmt::Assign` does not have a field named `name`
  --> src/llvm.rs:48:32
```

No se tocó `llvm.rs`. El otro agente ya usa `Stmt::Assign { target, value, .. }`.

## Verificación en el repo

```text
CARGO_TARGET_DIR=/tmp/sal-lambda-repo cargo test --offline --test semantics --test frontend --test parse_and_fmt
```

- semantics: **17 ok** (15 previos + `gpu_roundtrip_ok` + `lambda_move_string_then_use_is_emoved`)
- frontend: **11 ok**
- parse_and_fmt: **3 ok**

## Semántica lambda

- `Call` con callee `LAMBDA_CALLEE` (`"=>"`) → `Type::Fn` de un parámetro en infer/ownership/effects/device.
- `map(m, x => x * 2.0)` dentro de `on p` pasa los cuatro pases.
- `examples/gpu_roundtrip.sal` con lambda; `! gpu, alloc` (el plan omite `alloc`; el checker lo exige para `tensor[[…]]`).
