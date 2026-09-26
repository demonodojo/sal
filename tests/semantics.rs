use sal_compiler::ast::{Item, ParamMode};
use sal_compiler::device::check_devices;
use sal_compiler::diag::ErrorCode;
use sal_compiler::effects::check_effects;
use sal_compiler::fmt::format_program;
use sal_compiler::infer::infer_program;
use sal_compiler::ownership::{check_ownership, infer_param_modes};
use sal_compiler::parser::parse;

fn expect_code(src: &str, code: ErrorCode) {
    let err = run_semantics(src).expect_err(&format!("expected {code:?} for:\n{src}"));
    assert!(
        err.iter().any(|d| d.code == code),
        "expected {:?}, got {:?}: {:?}",
        code,
        err.iter().map(|d| d.code).collect::<Vec<_>>(),
        err.iter().map(|d| (&d.code, &d.message)).collect::<Vec<_>>(),
    );
}

fn expect_ok(src: &str) {
    if let Err(err) = run_semantics(src) {
        panic!(
            "expected ok, got {:?}: {:?}",
            err.iter().map(|d| d.code).collect::<Vec<_>>(),
            err.iter().map(|d| (&d.code, &d.message)).collect::<Vec<_>>(),
        );
    }
}

/// Parse then run the semantic passes used by compile (infer, effects, ownership, devices).
/// Parse-time diagnostics (e.g. E_TENSOR_ELEM) are returned as a single-element error list.
fn run_semantics(src: &str) -> Result<(), Vec<sal_compiler::diag::Diagnostic>> {
    let prog = match parse(src) {
        Ok(p) => p,
        Err(d) => return Err(vec![d]),
    };
    infer_program(&prog)?;
    check_effects(&prog)?;
    check_ownership(&prog)?;
    check_devices(&prog)?;
    Ok(())
}

#[test]
fn if_without_else_as_stmt_ok_for_int_return() {
    expect_ok(
        r#"
fn main() -> Int
    if true
        x = 1
    0
"#,
    );
}

#[test]
fn if_without_else_as_tail_rejects_non_unit_return() {
    expect_code(
        r#"
fn main() -> Int
    if true
        1
"#,
        ErrorCode::EType,
    );
}

#[test]
fn e_tensor_elem_rejects_float() {
    expect_code(
        r#"
fn f(x: Tensor[Float, 2, 2] on cpu) -> Int
    0
"#,
        ErrorCode::ETensorelem,
    );
}

#[test]
fn e_shape_matmul_mismatch() {
    expect_code(
        r#"
fn f(a: Tensor[F32, 2, 3] on cpu, b: Tensor[F32, 2, 2] on cpu) -> Tensor[F32, 2, 2] on cpu
    matmul(a, b)
"#,
        ErrorCode::EShape,
    );
}

#[test]
fn e_moved_string_after_bind() {
    expect_code(
        r#"
fn main() -> String
    s = "hi"
    t = s
    s
"#,
        ErrorCode::EMoved,
    );
}

#[test]
fn e_moved_tensor_after_to() {
    expect_code(
        r#"
fn main() -> Int ! gpu, alloc
    t = tensor[[1.0, 2.0], [3.0, 4.0]]
    g = to gpu t
    h = to gpu t
    0
"#,
        ErrorCode::EMoved,
    );
}

#[test]
fn copy_types_not_moved() {
    expect_ok(
        r#"
fn main() -> Int
    a = 1
    b = a
    c = 2.5
    d = c
    e = true
    f = e
    a + b
"#,
    );
}

#[test]
fn e_effect_gpu_without_decl() {
    expect_code(
        r#"
fn main() -> Int
    t = tensor[[1.0, 2.0], [3.0, 4.0]]
    g = to gpu t
    0
"#,
        ErrorCode::EEffect,
    );
}

#[test]
fn e_effect_alloc_without_decl() {
    expect_code(
        r#"
fn main() -> Int
    t = tensor[[1.0, 2.0], [3.0, 4.0]]
    0
"#,
        ErrorCode::EEffect,
    );
}

#[test]
fn e_effect_io_without_decl() {
    expect_code(
        r#"
fn main() -> Int
    w = load[F32, 4, 4]("w.salt")
    0
"#,
        ErrorCode::EEffect,
    );
}

#[test]
fn effect_gpu_with_to_ok() {
    expect_ok(
        r#"
fn main() -> Int ! gpu, alloc
    t = tensor[[1.0, 2.0], [3.0, 4.0]]
    g = to gpu t
    0
"#,
    );
}

#[test]
fn e_place_tensor_in_wrong_on() {
    // Ligar el `on` a un binding: una expresión suelta cierra el bloque como cola.
    expect_code(
        r#"
fn main() -> Int ! gpu, alloc
    host = tensor[[1.0, 2.0], [3.0, 4.0]]
    r = on gpu
        relu(host)
    0
"#,
        ErrorCode::EPlace,
    );
}

#[test]
fn e_place_matmul_different_places() {
    expect_code(
        r#"
fn f(a: Tensor[F32, 2, 2] on cpu, b: Tensor[F32, 2, 2] on gpu) -> Tensor[F32, 2, 2] on cpu
    matmul(a, b)
"#,
        ErrorCode::EPlace,
    );
}

#[test]
fn e_device_tpu_kernel() {
    expect_code(
        r#"
fn main() -> Int ! tpu
    on tpu kernel
        0
"#,
        ErrorCode::EDevice,
    );
}

#[test]
fn on_gpu_kernel_allowed() {
    expect_ok(
        r#"
fn main() -> Int ! gpu
    on gpu kernel
        0
"#,
    );
}

#[test]
fn hello_ok() {
    expect_ok(include_str!("../examples/hello.sal"));
}

#[test]
fn forward_ok() {
    expect_ok(include_str!("../examples/forward.sal"));
}

#[test]
fn gpu_roundtrip_ok() {
    expect_ok(include_str!("../examples/gpu_roundtrip.sal"));
}

#[test]
fn struct_field_int_ok() {
    let src = r#"
struct User
    age: Int

fn main(user: User) -> Int
    user.age
"#;
    let prog = parse(src).expect("parse");
    let out = infer_program(&prog).expect("infer");
    let (_, entries) = out
        .expr_types_by_fn
        .iter()
        .find(|(f, _)| f.name == "main")
        .expect("main");
    assert!(
        entries.iter().any(|e| {
            matches!(
                &e.ty,
                sal_compiler::ast::Type::Named { name, .. } if name == "Int"
            )
        }),
        "expected field type Int in expr_types, got {:?}",
        entries.iter().map(|e| &e.ty).collect::<Vec<_>>()
    );
    expect_ok(src);
}

#[test]
fn struct_unknown_field_is_etype() {
    expect_code(
        r#"
struct User
    age: Int

fn main(user: User) -> Int
    user.name
"#,
        ErrorCode::EType,
    );
}

#[test]
fn enum_option_match_ok() {
    expect_ok(
        r#"
enum Option[T]
    None
    Some(T)

fn main(o: Option[Int]) -> Int
    match o
        Some(x) => x
        None => 0
"#,
    );
}

#[test]
fn enum_unknown_variant_is_etype() {
    expect_code(
        r#"
enum Option[T]
    None
    Some(T)

fn main(o: Option[Int]) -> Int
    match o
        Maybe(x) => x
        None => 0
"#,
        ErrorCode::EType,
    );
}

#[test]
fn prelude_parses() {
    let src = include_str!("../std/prelude.sal");
    let prog = parse(src).expect("prelude should parse");
    assert!(prog.items.iter().any(|i| matches!(i, sal_compiler::ast::Item::Enum(e) if e.name == "Option")));
    assert!(prog.items.iter().any(|i| matches!(i, sal_compiler::ast::Item::Enum(e) if e.name == "Result")));
    infer_program(&prog).expect("prelude should typecheck");
}

#[test]
fn lambda_move_string_then_use_is_emoved() {
    expect_code(
        r#"
fn main() -> String
    s = "hi"
    f = x => consume(s)
    s
"#,
        ErrorCode::EMoved,
    );
}

#[test]
fn string_param_passed_to_consumer_is_take() {
    let src = r#"
fn sink(s: String) -> Int
    0

fn give(s: String) -> Int
    sink(s)
"#;
    let prog = infer_param_modes(&parse(src).expect("parse"));
    let give = prog
        .items
        .iter()
        .find_map(|i| match i {
            Item::Fn(f) if f.name == "give" => Some(f),
            _ => None,
        })
        .expect("give");
    assert_eq!(give.params[0].mode, ParamMode::Take);
    let formatted = format_program(&prog);
    assert!(
        formatted.contains("s: String take"),
        "expected take in formatted give, got:\n{formatted}"
    );
}

#[test]
fn tensor_param_matmul_is_borrow() {
    // matmul is a borrow_fn: args are not moved (no E_MOVED on reuse).
    let src = r#"
fn forward(x: Tensor[F32, 2, 2] on cpu, w: Tensor[F32, 2, 2] on cpu) -> Tensor[F32, 2, 2] on cpu
    matmul(x, w)
"#;
    expect_ok(src);
    let prog = infer_param_modes(&parse(src).expect("parse"));
    let forward = prog
        .items
        .iter()
        .find_map(|i| match i {
            Item::Fn(f) if f.name == "forward" => Some(f),
            _ => None,
        })
        .expect("forward");
    assert_eq!(forward.params[0].mode, ParamMode::Borrow);
    assert_eq!(forward.params[1].mode, ParamMode::Borrow);
    let formatted = format_program(&prog);
    assert!(
        formatted.contains("x: Tensor[F32, 2, 2] on cpu borrow"),
        "expected borrow on x, got:\n{formatted}"
    );
    assert!(
        formatted.contains("w: Tensor[F32, 2, 2] on cpu borrow"),
        "expected borrow on w, got:\n{formatted}"
    );
}

#[test]
fn int_param_stays_inferred() {
    let src = r#"
fn add1(n: Int) -> Int
    n + 1
"#;
    let prog = infer_param_modes(&parse(src).expect("parse"));
    let add1 = prog
        .items
        .iter()
        .find_map(|i| match i {
            Item::Fn(f) if f.name == "add1" => Some(f),
            _ => None,
        })
        .expect("add1");
    assert_eq!(add1.params[0].mode, ParamMode::Inferred);
    let formatted = format_program(&prog);
    assert!(
        !formatted.contains("borrow") && !formatted.contains("take"),
        "Int must not gain borrow/take, got:\n{formatted}"
    );
}

#[test]
fn fmt_param_modes_idempotent() {
    let src = r#"
fn sink(s: String) -> Int
    0

fn give(s: String) -> Int
    sink(s)

fn forward(x: Tensor[F32, 2, 2] on cpu, w: Tensor[F32, 2, 2] on cpu) -> Tensor[F32, 2, 2] on cpu
    matmul(x, w)

fn add1(n: Int) -> Int
    n + 1
"#;
    let f1 = format_program(&parse(src).expect("parse"));
    let f2 = format_program(&parse(&f1).expect("re-parse"));
    assert_eq!(f1, f2, "fmt not idempotent;\nfirst:\n{f1}\nsecond:\n{f2}");
    assert!(f1.contains("s: String take"));
    assert!(f1.contains("borrow"));
    assert!(f1.contains("n: Int)"));
}
