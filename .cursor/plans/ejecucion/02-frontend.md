# Plan: Frontend (02)

**Alcance fijo:** `src/lexer.rs`, `src/parser.rs`, `src/ast.rs`, `src/fmt.rs`, `src/span.rs`, `tests/parse_and_fmt.rs`, `tests/frontend.rs`.

**No tocar:** semántica, IR, runtime, CLI, `std/`, `examples/` (salvo que un test lea `examples/` en solo lectura).

**Firmas públicas a preservar:** `parse` (en `parser.rs`).

**Depende de:** SPEC.md (ya escrita).  
**Desbloquea:** B01, B02 → semántica, preludio, `gpu_roundtrip`.

---

## T01 — AST: lambda, decls de tipo, patrones ricos

- **Archivo:** `src/ast.rs`
- **Hacer:**
  - `Expr::Lambda { params: Vec<String>, body: Box<Expr>, span }`
  - `Item::Struct(StructDef)`, `Item::Enum(EnumDef)` con campos/variantes y genéricos de nombre
  - Extender `Pattern`: `Ctor { name, args, span }`, `List` si hace falta para el preludio
- **Hecho:** `serde` round-trip de un `Program` con esos nodos; `AST_SCHEMA_VERSION` sigue en 1 o se documenta bump en el plan de agentes (preferir v1 compatible añadiendo variantes).

## T02 — Lexer: tokens auxiliares si faltan

- **Archivo:** `src/lexer.rs`
- **Hacer:** keywords `struct`, `enum` (y `borrow`/`take` si se escriben en fuente; si solo salen del fmt tipado, pueden ser idents). Asegurar `FatArrow` usable fuera de `match`.
- **Hecho:** lex de `struct User` / `enum Option` / `x => x + 1` sin `E_PARSE` léxico.

## T03 — Parser: lambdas en argumentos

- **Archivo:** `src/parser.rs`
- **Hacer:** en args de llamada / primary, reconocer `ident (`,` ident)* => expr` como `Expr::Lambda`. Debe parsear `map(m, x => x * 2.0)`.
- **Hecho:** test en `tests/frontend.rs` `parse_gpu_roundtrip_ok` con `include_str!("../examples/gpu_roundtrip.sal")`.

## T04 — Parser: `struct` / `enum` / import path estable

- **Archivo:** `src/parser.rs`
- **Hacer:** ítems top-level `struct Name[T…]` / `enum Name[T…]` con cuerpo indentado; variantes `Name(types…)` o unit. Mantener `import`.
- **Hecho:** fixture en `tests/frontend.rs` con Option/Result mínimos parseados.

## T05 — Parser: patrones de constructor en `match`

- **Archivo:** `src/parser.rs`, `src/ast.rs`
- **Hacer:** `Some(x) =>`, `Ok(v) =>`, `Err(e) =>`, `_ =>`.
- **Hecho:** `tests/frontend.rs` `parse_match_ctors`.

## T06 — Formateador idempotente para lo nuevo

- **Archivo:** `src/fmt.rs`
- **Hacer:** imprimir lambdas `x => …`, structs/enums, modos `borrow`/`take` (ya parcial), `on`/`to`/`tensor`.
- **Hecho:** ampliar `tests/parse_and_fmt.rs` o `tests/frontend.rs`: `format_program(parse(s))` dos veces idéntico para `hello`, `forward`, `gpu_roundtrip` (cuando parsee).

## T07 — Spans coherentes

- **Archivo:** `src/span.rs` (+ usos en parser)
- **Hacer:** spans de lambda/struct/enum con `line`/`col` correctos para diagnósticos posteriores.
- **Hecho:** span del `=>` en lambda apunta a la línea del argumento en `gpu_roundtrip`.

## T08 — Tests frontend (nuevos)

- **Archivo:** `tests/frontend.rs` (crear), ampliar `tests/parse_and_fmt.rs`
- **Casos mínimos:**
  1. `parse_hello` / `fmt_idempotent` / `ast_json_roundtrip` (ya en parse_and_fmt; no romper).
  2. `parse_forward` conserva `on` + `relu(matmul(…))`.
  3. `parse_gpu_roundtrip_ok`.
  4. `parse_struct_enum_option_result`.
  5. `fmt_idempotent_gpu_roundtrip`.
  6. `reject_bad_indent` → `E_PARSE`.
- **Hecho:** `cargo test --test frontend --test parse_and_fmt` verde.

## Fuera de este plan

- Inferir tipos de lambdas → semántica.
- Bajar lambdas a IR → IR/runtime.
- Escribir el cuerpo de `std/prelude.sal` → superficie sal (autohospedaje/preludio).
