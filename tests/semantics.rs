use sal_compiler::ast::{Item, ParamMode};
use sal_compiler::device::check_devices;
use sal_compiler::diag::ErrorCode;
use sal_compiler::effects::check_effects;
use sal_compiler::fmt::format_program;
use sal_compiler::infer::infer_program;
use sal_compiler::modules::{check_module_semantics, resolve_module_graph};
use sal_compiler::ownership::{check_ownership, infer_param_modes};
use sal_compiler::parser::parse;
use std::fs;

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
fn elsif_without_else_is_unit() {
    expect_code(
        r#"
fn main() -> Int
    if true
        1
    elsif false
        2
"#,
        ErrorCode::EType,
    );
}

#[test]
fn elsif_with_else_has_then_type() {
    expect_ok(
        r#"
fn main() -> Int
    if true
        1
    elsif false
        2
    else
        3
"#,
    );
}

#[test]
fn elsif_without_else_as_stmt_ok() {
    expect_ok(
        r#"
fn main() -> Int
    if true
        x = 1
    elsif false
        x = 2
    0
"#,
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
fn dstring_requires_alloc_effect() {
    expect_code(
        r#"
fn main() -> String
    d"a"
"#,
        ErrorCode::EEffect,
    );
}

#[test]
fn dstring_ok_with_alloc() {
    expect_ok(
        r#"
fn main() -> String ! alloc
    d"a"
"#,
    );
}

#[test]
fn string_plus_concat_requires_alloc() {
    expect_code(
        r#"
fn main() -> String
    d"x" + d"y"
"#,
        ErrorCode::EEffect,
    );
}

#[test]
fn string_plus_concat_ok() {
    expect_ok(
        r#"
fn main() -> String ! alloc
    d"sum=" + int_to_str(10) + d"\n"
"#,
    );
}

#[test]
fn string_plus_mixed_type_rejects() {
    expect_code(
        r#"
fn main() -> String ! alloc
    1 + d"x"
"#,
        ErrorCode::EType,
    );
}

#[test]
fn string_plus_right_must_be_string() {
    expect_code(
        r#"
fn main() -> String ! alloc
    v = vec_new()
    _p = vec_push(v, d"z")
    d"a" + vec_get(v, 0)
"#,
        ErrorCode::EType,
    );
    expect_code(
        r#"
fn join(s: String, n: Int) -> String ! alloc
    s + n
"#,
        ErrorCode::EType,
    );
}

#[test]
fn string_plus_right_from_int_ok() {
    expect_ok(
        r#"
fn main() -> String ! alloc
    v = vec_new()
    _p = vec_push(v, d"z")
    d"a" + str_from_int(vec_get(v, 0))
"#,
    );
}

#[test]
fn string_plus_var_requires_alloc() {
    expect_code(
        r#"
fn join(s: String, n: String, m: String) -> String
    s + n + m
"#,
        ErrorCode::EEffect,
    );
}

#[test]
fn string_plus_var_ok() {
    expect_ok(
        r#"
fn join(s: String, n: String, m: String) -> String ! alloc
    s + n + m
"#,
    );
}

#[test]
fn string_plus_does_not_move_left() {
    expect_ok(
        r#"
fn twice(s: String) -> String ! alloc
    a = s + "x"
    b = s + "y"
    a + b
"#,
    );
}

#[test]
fn string_param_only_in_plus_is_borrow() {
    let prog = parse(
        r#"
fn show(s: String) -> String ! alloc
    s + "!"
"#,
    )
    .unwrap();
    let inferred = infer_param_modes(&prog);
    let Item::Fn(f) = &inferred.items[0] else { panic!("fn") };
    assert_eq!(f.params[0].mode, ParamMode::Borrow, "s: String borrow");
}

#[test]
fn string_accumulator_param_is_take() {
    let prog = parse(
        r#"
fn go(acc: String, piece: String) -> String ! alloc
    acc = acc + piece
    acc
"#,
    )
    .unwrap();
    let inferred = infer_param_modes(&prog);
    let Item::Fn(f) = &inferred.items[0] else { panic!("fn") };
    assert_eq!(f.params[0].mode, ParamMode::Take, "acc is appended in place");
    assert_eq!(f.params[1].mode, ParamMode::Borrow, "piece is only read");
}

#[test]
fn struct_string_field_plus_ok() {
    expect_ok(
        r#"
struct Msg
    text: String
    n: Int

fn show(m: Msg) -> String ! alloc
    m.text + "!"
"#,
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
fn int_param_rejects_string_without_bridge() {
    expect_code(
        r#"
fn sink(n: Int) -> Int
    n

fn main() -> Int
    sink("x")
"#,
        ErrorCode::EType,
    );
}

#[test]
fn let_int_from_string_binding_is_etype() {
    expect_code(
        r#"
fn main() -> Int
    s = d"hi"
    let n: Int = s
    0
"#,
        ErrorCode::EType,
    );
}

#[test]
fn unknown_type_name_is_etype() {
    expect_code(
        r#"
fn main(x: NotARealType) -> Int
    0
"#,
        ErrorCode::EType,
    );
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
fn lambda_string_handle_stays_usable() {
    expect_ok(
        r#"
fn main() -> String
    s = "hi"
    f = x => consume(s)
    s
"#,
    );
}

#[test]
fn string_param_passed_to_consumer_is_borrow() {
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
    assert_eq!(give.params[0].mode, ParamMode::Borrow);
    let formatted = format_program(&prog);
    assert!(
        formatted.contains("s: String borrow"),
        "expected borrow in formatted give, got:\n{formatted}"
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

#[test]
fn list_string_elem_typechecks() {
    expect_ok(
        r#"
fn main() -> Int ! alloc
    xs = list_new()
    xs = list_push(xs, 1)
    list_get(xs, 0)
"#,
    );
}

#[test]
fn dict_string_int_typechecks() {
    expect_ok(
        r#"
enum Option[T]
    None
    Some(T)

fn main() -> Int ! alloc
    d = dict_new()
    d = dict_put(d, "a", 1)
    d = dict_put(d, "b", 2)
    0
"#,
    );
}

#[test]
fn import_missing_path_is_etype() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = tmp.path().join("a.sal");
    fs::write(
        &a,
        "import \"./missing.sal\"\n\nfn main() -> Int\n    0\n",
    )
    .expect("write");
    match resolve_module_graph(&a, tmp.path()) {
        Err(err) => assert!(err.iter().any(|d| d.code == ErrorCode::EType)),
        Ok(_) => panic!("expected missing import to fail"),
    }
}

#[test]
fn import_cycle_is_etype() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let a = tmp.path().join("a.sal");
    let b = tmp.path().join("b.sal");
    fs::write(&b, "import \"./a.sal\"\n\nfn helper() -> Int\n    1\n").expect("write");
    fs::write(
        &a,
        "import \"./b.sal\"\n\nfn main() -> Int\n    helper()\n",
    )
    .expect("write");
    match resolve_module_graph(&a, tmp.path()) {
        Err(err) => assert!(err.iter().any(|d| d.code == ErrorCode::EType)),
        Ok(_) => panic!("expected import cycle to fail"),
    }
}

#[test]
fn import_call_typechecks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let b = tmp.path().join("b.sal");
    let a = tmp.path().join("a.sal");
    fs::write(&b, "fn helper() -> Int\n    7\n").expect("write");
    fs::write(
        &a,
        "import \"./b.sal\"\n\nfn main() -> Int\n    helper()\n",
    )
    .expect("write");
    let graph = resolve_module_graph(&a, tmp.path()).expect("graph");
    let root = graph.order.last().expect("root");
    check_module_semantics(root, &graph).expect("imported helper visible");
}

#[test]
fn string_to_gpu_is_e_place() {
    expect_code(
        r#"
fn main() -> Int ! gpu, alloc
    s = to gpu "hi"
    0
"#,
        ErrorCode::EPlace,
    );
}

#[test]
fn str_bytes_typechecks() {
    expect_ok(
        r#"
fn main() -> Tensor[I8, ?] on cpu ! alloc
    str_bytes("abc")
"#,
    );
}
