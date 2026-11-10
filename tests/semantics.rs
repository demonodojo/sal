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
    let mut prog = match parse(src) {
        Ok(p) => p,
        Err(d) => return Err(vec![d]),
    };
    sal_compiler::methods::prepare_program(&mut prog)?;
    let infer = infer_program(&prog)?;
    check_effects(&prog)?;
    check_ownership(&prog, &infer)?;
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
fn string_param_read_by_plus_is_take() {
    let prog = parse(
        r#"
fn show(s: String) -> String ! alloc
    s + "!"
"#,
    )
    .unwrap();
    let inferred = infer_param_modes(&prog);
    let Item::Fn(f) = &inferred.items[0] else { panic!("fn") };
    // SPEC «Memoria»: a by-value `String` moves into the fn; `+` only reads its operands.
    assert_eq!(f.params[0].mode, ParamMode::Take, "s: String moves");
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
    assert_eq!(f.params[1].mode, ParamMode::Take, "piece moves in: by-value String");
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
fn consume(s: &String) -> Int
    str_len(s)

fn main() -> String
    s = "hi"
    f = x => consume(&s)
    s
"#,
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
        !formatted.contains("take"),
        "mode words are not printed (SPEC «Memoria»), got:\n{formatted}"
    );
}

#[test]
fn tensor_param_matmul_is_take() {
    // A by-value Tensor parameter moves into the fn (SPEC «Memoria»).
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
    assert_eq!(forward.params[0].mode, ParamMode::Take);
    assert_eq!(forward.params[1].mode, ParamMode::Take);
    let formatted = format_program(&prog);
    assert!(!formatted.contains("borrow") && !formatted.contains("take"), "{formatted}");
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
    // Mode words are not source syntax (SPEC «Memoria»): the formatter prints none.
    assert!(!f1.contains("take") && !f1.contains("borrow"), "{f1}");
    assert!(f1.contains("n: Int)"));
}

#[test]
fn list_string_elem_typechecks() {
    expect_ok(
        r#"
fn main() -> Int ! alloc
    xs = list_new[Int]()
    list_push(xs, 1)
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
    d = dict_new[String, Int]()
    dict_put(d, "a", 1)
    dict_put(d, "b", 2)
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
    check_module_semantics(root, &graph, sal_compiler::target::Target::host())
        .expect("imported helper visible");
}

#[test]
fn import_as_avoids_flat_name_clash() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let left = tmp.path().join("left.sal");
    let right = tmp.path().join("right.sal");
    let main = tmp.path().join("main.sal");
    fs::write(&left, "fn helper() -> Int\n    1\n").expect("write left");
    fs::write(&right, "fn helper() -> Int\n    2\n").expect("write right");
    fs::write(
        &main,
        "import \"./left.sal\" as L\nimport \"./right.sal\" as R\n\nfn main() -> Int\n    L.helper()\n",
    )
    .expect("write main");
    let graph = resolve_module_graph(&main, tmp.path()).expect("graph");
    let root = graph.order.last().expect("root");
    check_module_semantics(root, &graph, sal_compiler::target::Target::host())
        .expect("qualified imports resolve");
}

#[test]
fn flat_import_same_name_still_clashes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let left = tmp.path().join("left.sal");
    let right = tmp.path().join("right.sal");
    let main = tmp.path().join("main.sal");
    fs::write(&left, "fn helper() -> Int\n    1\n").expect("write left");
    fs::write(&right, "fn helper() -> Int\n    2\n").expect("write right");
    fs::write(
        &main,
        "import \"./left.sal\"\nimport \"./right.sal\"\n\nfn main() -> Int\n    helper()\n",
    )
    .expect("write main");
    match resolve_module_graph(&main, tmp.path()) {
        Err(err) => assert!(err.iter().any(|d| d.code == ErrorCode::EType)),
        Ok(_) => panic!("expected flat import name clash"),
    }
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

#[test]
fn tensor_mul_scalar_typechecks() {
    expect_ok(
        r#"
fn f(v: Tensor[F32, 4] on p) -> Tensor[F32, 4] on p
    on p
        v * 2.0
"#,
    );
}

#[test]
fn tensor_place_mismatch_is_e_place() {
    expect_code(
        r#"
fn f(a: Tensor[F32, 4] on cpu, b: Tensor[F32, 4] on gpu) -> Tensor[F32, 4] on cpu
    a + b
"#,
        ErrorCode::EPlace,
    );
}

#[test]
fn frame_with_string_parses_and_typechecks() {
    expect_ok(
        r#"
frame Shop
    price: F32
    name: String

fn main() -> Int ! alloc
    0
"#,
    );
}

#[test]
fn frame_with_string_to_gpu_is_e_place() {
    expect_code(
        r#"
frame Shop
    price: F32
    name: String

fn go(s: Shop) -> Int ! gpu
    to gpu s
    0
"#,
        ErrorCode::EPlace,
    );
}

#[test]
fn int_bitwise_and_shift_typecheck() {
    expect_ok(
        r#"
fn main() -> Int
    1 << 2 | 3 & 7 ^ 1
"#,
    );
}

#[test]
fn int_mod_requires_int() {
    expect_code(
        r#"
fn main() -> Int
    1.0 % 2
"#,
        ErrorCode::EType,
    );
}

#[test]
fn bytes_new_and_get_typecheck() {
    expect_ok(
        r#"
fn main() -> Int ! alloc
    b = bytes_new(4)
    bytes_set_u8(b, 0, 10)
    bytes_get_u8(b, 0)
"#,
    );
}

#[test]
fn syscall_is_ok_on_linux_host() {
    expect_ok(
        r#"
fn main() -> Int ! io
    syscall(0, 0, 0, 0, 0, 0, 0)
"#,
    );
}

#[test]
fn early_return_in_if_ok() {
    expect_ok(
        r#"
fn pick(x: Int) -> Int
    if x == 0
        return 1
    99
"#,
    );
}

#[test]
fn return_type_mismatch() {
    expect_code(
        r#"
fn main() -> Int
    return true
"#,
        ErrorCode::EType,
    );
}

#[test]
fn bare_return_on_non_unit_fn() {
    expect_code(
        r#"
fn main() -> Int
    return
"#,
        ErrorCode::EType,
    );
}

#[test]
fn unreachable_after_return() {
    expect_code(
        r#"
fn main() -> Int
    return 1
    2
"#,
        ErrorCode::EType,
    );
}

#[test]
fn return_inside_on_rejected() {
    expect_code(
        r#"
fn main() -> Int
    on cpu
        return 1
    0
"#,
        ErrorCode::EType,
    );
}

#[test]
fn fn_missing_return_value() {
    expect_code(
        r#"
fn main() -> Int
    x = 1
"#,
        ErrorCode::EType,
    );
}

#[test]
fn string_passed_to_taking_fn_is_moved() {
    expect_code(
        r#"
fn consume(s: String) -> Int
    0

fn main() -> Int
    s = "hi"
    n = consume(s)
    n + str_len(s)
"#,
        ErrorCode::EMoved,
    );
}

#[test]
fn string_pushed_into_list_is_moved() {
    expect_code(
        r#"
fn main() -> Int ! alloc
    xs = list_new[String]()
    s = int_to_str(7)
    _a = list_push(xs, s)
    str_len(s)
"#,
        ErrorCode::EMoved,
    );
}

#[test]
fn argument_of_a_constructor_is_moved() {
    expect_code(
        r#"
struct Tag
    name: String
    n: Int

fn main() -> Int ! alloc
    s = int_to_str(7)
    t = Tag(s, 1)
    str_len(s)
"#,
        ErrorCode::EMoved,
    );
}

#[test]
fn take_mode_flows_through_a_forwarding_fn() {
    expect_code(
        r#"
fn eat(s: String) -> Int
    0

fn forward(s: String) -> Int
    eat(s)

fn main() -> Int
    s = "hi"
    n = forward(s)
    n + str_len(s)
"#,
        ErrorCode::EMoved,
    );
}

#[test]
fn copy_struct_stays_usable_after_a_move_position() {
    expect_ok(
        r#"
struct P
    x: Int
    y: Int

fn main() -> Int
    p = P(1, 2)
    q = p
    p.x + q.y
"#,
    );
}

#[test]
fn moving_a_field_moves_the_whole_afterwards() {
    expect_code(
        r#"
struct P
    name: String
    other: String

fn main() -> Int ! alloc
    p = P(strdup(d"a"), strdup(d"b"))
    n = p.name
    str_len(n) + str_len(p)
"#,
        ErrorCode::EMoved,
    );
}

#[test]
fn field_assignment_puts_a_moved_field_back() {
    expect_ok(
        r#"
struct P
    name: String
    other: String

fn main() -> Int ! alloc
    p = P(strdup(d"a"), strdup(d"b"))
    n = p.name
    p.name = strdup(d"c")
    str_len(n) + str_len(p.name)
"#,
    );
}

#[test]
fn method_call_resolves_by_receiver_type() {
    expect_ok(
        "struct P\n    x: Int\n\nmethod P.get() -> Int\n    self.x\n\nfn main() -> Int\n    P(1).get()\n",
    );
}

#[test]
fn method_on_wrong_receiver_type_is_e_type() {
    expect_code(
        "struct P\n    x: Int\n\nstruct Q\n    x: Int\n\nmethod P.get() -> Int\n    self.x\n\nfn main() -> Int\n    Q(1).get()\n",
        ErrorCode::EType,
    );
}

#[test]
fn method_name_clash_with_mangled_fn_is_e_type() {
    expect_code(
        "struct P\n    x: Int\n\nfn P__get(p: P) -> Int\n    p.x\n\nmethod P.get() -> Int\n    self.x\n\nfn main() -> Int\n    0\n",
        ErrorCode::EType,
    );
}

#[test]
fn double_underscore_in_method_name_is_e_type() {
    expect_code(
        "struct P\n    x: Int\n\nmethod P.a__b() -> Int\n    self.x\n\nfn main() -> Int\n    0\n",
        ErrorCode::EType,
    );
}

#[test]
fn method_on_generic_receiver_is_e_type() {
    expect_code(
        "struct B[T]\n    x: Int\n\nmethod B.get() -> Int\n    self.x\n\nfn main() -> Int\n    0\n",
        ErrorCode::EType,
    );
}

#[test]
fn static_and_init_calls_resolve_by_type_name() {
    expect_ok(
        "struct P\n    x: Int\n\ninit P(x: Int)\n    P(x)\n\nstatic P.zero() -> P\n    P(0)\n\nfn main() -> Int\n    P.new(1).x + P.zero().x\n",
    );
}

#[test]
fn a_local_shadows_the_type_name_for_calls() {
    expect_code(
        "struct P\n    x: Int\n\nstatic P.zero() -> P\n    P(0)\n\nfn main() -> Int\n    P = 1\n    P.zero().x\n",
        ErrorCode::EType,
    );
}

#[test]
fn static_on_a_value_and_instance_on_a_type_are_e_type() {
    expect_code(
        "struct P\n    x: Int\n\nstatic P.zero() -> P\n    P(0)\n\nfn main() -> Int\n    P(1).zero().x\n",
        ErrorCode::EType,
    );
    expect_code(
        "struct P\n    x: Int\n\nmethod P.get() -> Int\n    self.x\n\nfn main() -> Int\n    P.get()\n",
        ErrorCode::EType,
    );
}

#[test]
fn init_and_static_new_clash() {
    expect_code(
        "struct P\n    x: Int\n\ninit P(x: Int)\n    P(x)\n\nstatic P.new(x: Int) -> P\n    P(x)\n\nfn main() -> Int\n    0\n",
        ErrorCode::EType,
    );
}

#[test]
fn builtin_methods_type_like_the_builtin_call() {
    expect_ok(
        "fn main() -> Int ! alloc\n    xs = list_new[Int]()\n    xs.push(1)\n    xs.len() + \"ab\".len()\n",
    );
}

#[test]
fn builtin_method_errors_are_e_type() {
    expect_code("fn main() -> Int\n    \"ab\".nope()\n", ErrorCode::EType);
    expect_code(
        "fn main() -> Int ! alloc\n    xs = list_new[Int]()\n    xs.push(\"a\")\n    0\n",
        ErrorCode::EType,
    );
}

#[test]
fn builtin_method_table_has_the_documented_entries() {
    use sal_compiler::methods::builtin_method;
    assert_eq!(builtin_method("String", "len"), Some("str_len"));
    assert_eq!(builtin_method("String", "dup"), Some("strdup"));
    assert_eq!(builtin_method("List", "push"), Some("list_push"));
    assert_eq!(builtin_method("Dict", "has"), Some("dict_has"));
    assert_eq!(builtin_method("Bytes", "len"), Some("bytes_len"));
    assert_eq!(builtin_method("Int", "to_str"), Some("int_to_str"));
    assert_eq!(builtin_method("String", "nope"), None);
}

#[test]
fn builtin_constructors_need_their_type_arguments() {
    expect_ok("fn main() -> Int ! alloc\n    xs = List.new[Int]()\n    xs.len()\n");
    expect_code("fn main() -> Int ! alloc\n    xs = List.new()\n    xs.len()\n", ErrorCode::EType);
    expect_code("fn main() -> Int ! alloc\n    d = Dict.new[Int]()\n    0\n", ErrorCode::EType);
    expect_code("fn main() -> Int ! alloc\n    d = List.nope[Int]()\n    0\n", ErrorCode::EType);
}

#[test]
fn float_math_methods_take_and_return_float() {
    expect_ok("fn main() -> Int\n    x = 4.0\n    x.sqrt().pow(2.0).to_int()\n");
    expect_code("fn main() -> Int\n    float_sqrt(4)\n", ErrorCode::EType);
    expect_code("fn main() -> Int\n    n = 4\n    n.sqrt().to_int()\n", ErrorCode::EType);
    expect_code("fn main() -> Int\n    x = 4.0\n    x.pow().to_int()\n", ErrorCode::EType);
}

#[test]
fn float_math_table_entries() {
    use sal_compiler::methods::builtin_method;
    assert_eq!(builtin_method("Float", "sqrt"), Some("float_sqrt"));
    assert_eq!(builtin_method("Float", "pow"), Some("float_pow"));
    assert_eq!(builtin_method("Float", "to_int"), Some("float_to_int"));
    assert_eq!(builtin_method("Float", "to_str"), Some("f64_bits_to_str"));
    assert_eq!(builtin_method("Int", "sqrt"), None);
}

#[test]
fn lifetime_elision_follows_the_three_rules() {
    expect_ok("fn id(s: &String) -> &String\n    s\n");
    expect_ok("fn both(a: &String, b: &String) -> Int\n    a.len() + b.len()\n");
    expect_code(
        "fn pick(a: &String, b: &String) -> &String\n    a\n",
        ErrorCode::EBorrow,
    );
    expect_code(
        "fn missing(s: &'a String) -> &'a String\n    s\n",
        ErrorCode::EType,
    );
    expect_ok("fn named['a](s: &'a String) -> &'a String\n    s\n");
}

#[test]
fn clone_is_a_method_and_tensor_has_none() {
    expect_ok("fn main() -> Int ! alloc\n    s = \"ab\"\n    t = s.clone()\n    t.len()\n");
    expect_code(
        "fn main() -> Int\n    m = tensor[[1.0, 2.0], [3.0, 4.0]]\n    m.clone()\n",
        ErrorCode::EType,
    );
}

// SPEC «Memoria»: a container that receives a value by `push` owns it. The borrow checker
// publishes the locals each fn moves, which `elaborate_drops` reads (subplan 13).
#[test]
fn moved_locals_publishes_the_value_pushed_into_a_list() {
    let mut prog = parse(
        "fn main() -> Int ! alloc\n    xs = List.new[List[Int]]()\n    ys = List.new[Int]()\n    xs.push(ys)\n    xs.len()\n",
    )
    .expect("parse");
    // The pipeline resolves `xs.push(ys)` to `list_push(xs, ys)` before any pass (SPEC «Métodos»).
    sal_compiler::methods::prepare_program(&mut prog).expect("methods");
    let infer = infer_program(&prog).expect("infer");
    let moved = sal_compiler::borrowck::moved_locals(&prog, &infer);
    let main = moved.get("main").expect("main");
    assert!(main.contains("ys"), "ys moves into xs: {main:?}");
    assert!(!main.contains("xs"), "xs is not moved: {main:?}");
}

// SPEC «Memoria»: `push` moves its value. Reading the moved local afterwards is `E_MOVED`.
#[test]
fn value_used_after_push_is_e_moved() {
    expect_code(
        "fn main() -> Int ! alloc\n    xs = List.new[String]()\n    s = strdup(d\"ab\")\n    xs.push(s)\n    str_len(s)\n",
        ErrorCode::EMoved,
    );
}

// SPEC «Memoria»: a struct constructor moves its arguments into the value. The borrow checker
// publishes them, so the drops pass does not release a list the struct already owns.
#[test]
fn moved_locals_includes_constructor_arguments() {
    let mut prog = parse(
        "struct P\n    xs: List[Int]\n    n: Int\n\nfn mk() -> P ! alloc\n    xs = list_new[Int]()\n    P(xs, 1)\n",
    )
    .expect("parse");
    sal_compiler::methods::prepare_program(&mut prog).expect("methods");
    let infer = infer_program(&prog).expect("infer");
    let moved = sal_compiler::borrowck::moved_locals(&prog, &infer);
    assert!(moved.get("mk").is_some_and(|m| m.contains("xs")), "{moved:?}");
}

// SPEC «Memoria»: a struct may hold a borrow (`struct V['a]`), and a use of it spells the lifetime
// (`View['a]` is not a type argument: it is dropped, the reference carries it).
#[test]
fn struct_with_borrowed_field_takes_a_lifetime_at_use() {
    expect_ok(
        "struct Items\n    xs: List[Int]\n\nstruct View['a]\n    items: &'a List[Int]\n    n: Int\n\nfn view_of['a](it: &'a Items) -> View\n    View(&it.xs, it.xs.len())\n\nfn main() -> Int ! alloc\n    it = Items(list_new[Int]())\n    v = view_of(&it)\n    v.n\n",
    );
}
