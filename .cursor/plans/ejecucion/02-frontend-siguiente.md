# Frontend — trabajo fuera de la franja 02

El lexer/parser/fmt de arranque cubre la gramática de SPEC en `tests/frontend.rs`.
Quedó aplazado lo que rompería módulos fuera de los archivos permitidos
(`src/lexer.rs`, `src/parser.rs`, `src/ast.rs`, `src/fmt.rs`, `src/span.rs`,
`tests/parse_and_fmt.rs`, `tests/frontend.rs`).

## Bloqueos por matches exhaustivos

1. **`Item::Struct` / `Item::Enum`**  
   **Hecho:** variantes `Item::Struct(StructDef)` e `Item::Enum(EnumDef)` en el AST;
   el parser las emite en `Program.items` (no en `type_defs`); formateador imprime
   desde `items`; `typed.rs` las registra como `TypedItem::Struct` / `Enum`.
   `type_defs` queda con `#[serde(default)]` vacío en el camino nuevo (JSON viejo).

2. **`Expr::Lambda`**  
   **Hecho:** variante `Expr::Lambda { params, body, span }` en el AST;
   el parser emite `Lambda` (no `Call` con callee `"=>"`); formateador
   imprime `x => …`; tipado en infer/ownership/effects/device trata la
   lambda como `Type::Fn`, no como llamada. Brazos mínimos en ir/llvm.

## Huecos de superficie menores

- **`if` / `else`:** token `If` en el lexer; sin parseo aún (SPEC usa `match`).
- **Tipo función en posición de tipo** (`fn(Int) -> Bool ! io`): existe
  `Type::Fn` en el AST; el parser aún no lo lee desde fuente.
- **Interpolación:** solo `{ident}` simple; no expresiones arbitrarias dentro
  de `{}`. El literal completo sigue en `Expr::String.value`; las partes
  estructuradas van en `parts`.
- **Lambdas multi-param** `(x, y) => …`: no implementado.
- **Protocolos / `any Protocol`:** fuera del arranque según el plan.

## JSON schema

`Program.schema_version` (= `AST_SCHEMA_VERSION`, hoy `1`) ya existía y el
round-trip de `tests/parse_and_fmt.rs` / `tests/frontend.rs` lo conserva.
Campos nuevos con `#[serde(default)]`: `type_defs`, `FnDef.type_params`,
`Expr::String.parts`.
