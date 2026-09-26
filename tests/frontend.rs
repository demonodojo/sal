//! Frontend grammar coverage for the sal bootstrap parser/fmt.
//! No network; pure parse + format checks.

use sal_compiler::ast::{
    BinOp, Dim, Effect, Expr, Item, Pattern, Place, StringPart, TensorElem, Type, TypeArg,
};
use sal_compiler::fmt::format_program;
use sal_compiler::parser::parse;

fn assert_parse(src: &str) -> sal_compiler::ast::Program {
    parse(src).unwrap_or_else(|e| panic!("parse failed: {} at {}:{}", e.message, e.span.line, e.span.col))
}

fn assert_fmt_idempotent(src: &str) {
    let p1 = assert_parse(src);
    let f1 = format_program(&p1);
    let p2 = assert_parse(&f1);
    let f2 = format_program(&p2);
    assert_eq!(f1, f2, "fmt not idempotent;\nfirst:\n{f1}\nsecond:\n{f2}");
}

#[test]
fn examples_parse() {
    for name in ["hello", "forward", "gpu_roundtrip"] {
        let src = std::fs::read_to_string(format!("examples/{name}.sal")).unwrap();
        let p = assert_parse(&src);
        assert!(!p.items.is_empty(), "{name} has no items");
        assert_eq!(p.schema_version, 2);
    }
}

#[test]
fn comments_and_indent_blocks() {
    let src = "\
# top comment
fn main() -> Int
    # inside
    1
    # mid
    2
";
    let p = assert_parse(src);
    let Item::Fn(f) = &p.items[0] else { panic!("expected fn") };
    assert_eq!(f.body.stmts.len(), 1);
    assert!(matches!(f.body.tail.as_deref(), Some(Expr::Int { value: 2, .. })));
}

#[test]
fn return_and_tail_expr() {
    let src = "\
fn early() -> Int
    return 1
    99

fn late() -> Int
    42
";
    let p = assert_parse(src);
    assert_eq!(p.items.len(), 2);
    let Item::Fn(early) = &p.items[0] else { panic!() };
    assert!(matches!(
        &early.body.stmts[0],
        sal_compiler::ast::Stmt::Return {
            value: Some(Expr::Int { value: 1, .. }),
            ..
        }
    ));
    let Item::Fn(late) = &p.items[1] else { panic!() };
    assert!(matches!(late.body.tail.as_deref(), Some(Expr::Int { value: 42, .. })));
}

#[test]
fn types_scalars_list_tensor() {
    let src = "\
fn f(
    a: Int,
    b: Float,
    c: Bool,
    d: String,
    e: Unit,
    xs: List[Int],
    t: Tensor[F32, ?, 4] on p,
    g: Tensor[F16, 2, 2] on gpu,
    h: Tensor[BF16, 1] on cpu,
    i: Tensor[I8, 3] on tpu
) -> Unit
    e
";
    let p = assert_parse(src);
    let Item::Fn(f) = &p.items[0] else { panic!() };
    assert_eq!(f.params.len(), 10);
    assert!(matches!(
        &f.params[5].ty,
        Type::Named { name, args, .. } if name == "List" && args.len() == 1
    ));
    assert!(matches!(
        &f.params[6].ty,
        Type::Tensor {
            elem: TensorElem::F32,
            dims,
            place: Place::Param(p),
            ..
        } if dims == &vec![Dim::Dynamic, Dim::Static(4)] && p == "p"
    ));
    assert!(matches!(
        &f.params[7].ty,
        Type::Tensor {
            elem: TensorElem::F16,
            place: Place::Gpu,
            ..
        }
    ));
}

#[test]
fn effects_all() {
    let src = "\
fn main() -> Int ! io, alloc, panic, gpu, tpu
    0
";
    let p = assert_parse(src);
    let Item::Fn(f) = &p.items[0] else { panic!() };
    assert_eq!(
        f.effects,
        vec![
            Effect::Io,
            Effect::Alloc,
            Effect::Panic,
            Effect::Gpu,
            Effect::Tpu
        ]
    );
}

#[test]
fn on_to_kernel_and_tensor_lit() {
    let src = "\
fn scale(m: Tensor[F32, 2, 2] on p) -> Tensor[F32, 2, 2] on p
    on p
        map(m, x => x * 2.0)

fn kern() -> Int ! gpu
    on gpu kernel
        0

fn round() -> Int ! gpu
    host = tensor[[1.0, 2.0], [3.0, 4.0]]
    dev = to gpu host
    back = to cpu scale(dev)
    t = to tpu back
    0
";
    let p = assert_parse(src);
    assert_eq!(p.items.len(), 3);
    let Item::Fn(scale) = &p.items[0] else { panic!() };
    let Some(Expr::On { place, kernel, body, .. }) = scale.body.tail.as_deref() else {
        panic!("expected on tail");
    };
    assert!(matches!(place, Place::Param(n) if n == "p"));
    assert!(!kernel);
    let Some(Expr::Call { args, .. }) = body.tail.as_deref() else {
        panic!("expected map call");
    };
    assert!(matches!(
        &args[1],
        Expr::Lambda {
            params,
            body,
            ..
        } if params.as_slice() == ["x"]
            && matches!(body.as_ref(), Expr::Binary { .. })
    ));
    let Item::Fn(kern) = &p.items[1] else { panic!() };
    assert!(matches!(
        kern.body.tail.as_deref(),
        Some(Expr::On { kernel: true, place: Place::Gpu, .. })
    ));
}

#[test]
fn match_try_calls_load() {
    let src = "\
fn go(r: Result[Int, String]) -> Int ! io, alloc
    x = try r
    y = load[F32, 4, 4](\"w.salt\")
    match x
        Some(v) => v
        None => 0
        _ => 1
";
    let p = assert_parse(src);
    let Item::Fn(f) = &p.items[0] else { panic!() };
    assert!(matches!(
        &f.body.stmts[0],
        sal_compiler::ast::Stmt::Assign {
            value: Expr::Try { .. },
            ..
        }
    ));
    assert!(matches!(
        &f.body.stmts[1],
        sal_compiler::ast::Stmt::Assign {
            value: Expr::Call { type_args, .. },
            ..
        } if type_args.len() == 3
            && matches!(&type_args[0], TypeArg::Type(Type::Named { name, .. }) if name == "F32")
            && matches!(&type_args[1], TypeArg::Dim(Dim::Static(4)))
            && matches!(&type_args[2], TypeArg::Dim(Dim::Static(4)))
    ));
    let Some(Expr::Match { arms, .. }) = f.body.tail.as_deref() else {
        panic!("expected match");
    };
    assert!(matches!(
        &arms[0].pattern,
        Pattern::Variant { name, args, .. } if name == "Some" && args.len() == 1
    ));
    assert!(matches!(&arms[1].pattern, Pattern::Ident(n, _) if n == "None"));
    assert!(matches!(&arms[2].pattern, Pattern::Wild(_)));
}

#[test]
fn string_interpolation() {
    let src = "\
fn greet(name: String) -> String
    \"Hello, {name}\"
";
    let p = assert_parse(src);
    let Item::Fn(f) = &p.items[0] else { panic!() };
    let Some(Expr::String { value, parts, .. }) = f.body.tail.as_deref() else {
        panic!("expected string");
    };
    assert_eq!(value, "Hello, {name}");
    assert_eq!(
        parts,
        &vec![
            StringPart::Lit("Hello, ".into()),
            StringPart::Interp("name".into())
        ]
    );
}

#[test]
fn struct_enum_and_generics() {
    let src = "\
struct User
    name: String
    id: Int

enum Option[T]
    None
    Some(T)

fn id[T](x: T) -> T
    x
";
    let p = assert_parse(src);
    assert!(p.type_defs.is_empty());
    assert_eq!(p.items.len(), 3);
    assert!(matches!(
        &p.items[0],
        Item::Struct(s) if s.name == "User" && s.fields.len() == 2
    ));
    assert!(matches!(
        &p.items[1],
        Item::Enum(e) if e.name == "Option"
            && e.type_params == vec!["T".to_string()]
            && e.variants.len() == 2
    ));
    let Item::Fn(f) = &p.items[2] else { panic!() };
    assert_eq!(f.type_params, vec!["T".to_string()]);
}

#[test]
fn fmt_idempotent_examples_and_surface() {
    for name in ["hello", "forward", "gpu_roundtrip"] {
        let src = std::fs::read_to_string(format!("examples/{name}.sal")).unwrap();
        assert_fmt_idempotent(&src);
    }
    assert_fmt_idempotent(
        "\
fn greet(name: String) -> String
    \"Hello, {name}\"
",
    );
    assert_fmt_idempotent(
        "\
struct Point
    x: Int
    y: Int

fn id[T](x: T) -> T
    x
",
    );
}

#[test]
fn ast_json_roundtrip_with_schema() {
    let src = std::fs::read_to_string("examples/gpu_roundtrip.sal").unwrap();
    let p = assert_parse(&src);
    assert_eq!(p.schema_version, 2);
    let j = serde_json::to_string(&p).unwrap();
    assert!(j.contains("\"schema_version\":2") || j.contains("\"schema_version\": 2"));
    let p2: sal_compiler::ast::Program = serde_json::from_str(&j).unwrap();
    assert_eq!(p, p2);
}

#[test]
fn struct_enum_json_roundtrip() {
    let src = "\
struct User
    name: String
    id: Int

enum Option[T]
    None
    Some(T)

fn id[T](x: T) -> T
    x
";
    let p = assert_parse(src);
    assert!(matches!(&p.items[0], Item::Struct(s) if s.name == "User"));
    assert!(matches!(&p.items[1], Item::Enum(e) if e.name == "Option"));
    let j = serde_json::to_string(&p).unwrap();
    assert!(j.contains("\"Struct\"") || j.contains("Struct"));
    let p2: sal_compiler::ast::Program = serde_json::from_str(&j).unwrap();
    assert_eq!(p, p2);
    assert!(p2.type_defs.is_empty());
}

fn tail_of(src: &str) -> Expr {
    let p = assert_parse(src);
    let Item::Fn(f) = &p.items[0] else { panic!("expected fn") };
    f.body.tail.as_deref().unwrap().clone()
}

#[test]
fn left_recursive_ops_and_postfix() {
    match tail_of("fn f() -> Int\n    1 - 2 - 3\n") {
        Expr::Binary { op: BinOp::Sub, left, right, .. } => {
            assert!(matches!(
                left.as_ref(),
                Expr::Binary { op: BinOp::Sub, .. }
            ));
            assert!(matches!(right.as_ref(), Expr::Int { value: 3, .. }));
        }
        other => panic!("expected left-assoc sub, got {other:?}"),
    }
    match tail_of("fn f() -> Int\n    a.b.c\n") {
        Expr::Field { field, base, .. } => {
            assert_eq!(field, "c");
            assert!(matches!(base.as_ref(), Expr::Field { field, .. } if field == "b"));
        }
        other => panic!("expected left-assoc field, got {other:?}"),
    }
    match tail_of("fn f() -> Int\n    f(x)(y)\n") {
        Expr::Call { args, func, .. } => {
            assert!(matches!(args[0], Expr::Ident { ref name, .. } if name == "y"));
            assert!(matches!(func.as_ref(), Expr::Call { .. }));
        }
        other => panic!("expected left-assoc call, got {other:?}"),
    }
}

#[test]
fn lambda_is_right_recursive_and_body_is_expr() {
    match tail_of("fn f() -> Int\n    x => y => z\n") {
        Expr::Lambda { params, body, .. } => {
            assert_eq!(params, vec!["x".to_string()]);
            assert!(matches!(
                body.as_ref(),
                Expr::Lambda { params, .. } if params == &vec!["y".to_string()]
            ));
        }
        other => panic!("expected nested lambda, got {other:?}"),
    }
    assert!(matches!(
        tail_of("fn f() -> Int\n    x => try x\n"),
        Expr::Lambda { body, .. } if matches!(body.as_ref(), Expr::Try { .. })
    ));
    let err = parse("fn f() -> Int\n    a + b => c\n").unwrap_err();
    assert_eq!(err.code.as_str(), "E_PARSE");
}

#[test]
fn import_path_is_left_recursive() {
    let p = assert_parse("import pkg.mod.item\n");
    assert!(matches!(&p.items[0], Item::Import(i) if i.path == "pkg.mod.item"));
    let p = assert_parse("import \"std/prelude.sal\"\n");
    assert!(matches!(&p.items[0], Item::Import(i) if i.path == "std/prelude.sal"));
}

#[test]
fn tab_at_and_parallel_are_parse_errors() {
    assert_eq!(parse("fn f() -> Int\n\t1\n").unwrap_err().code.as_str(), "E_PARSE");
    assert_eq!(parse("fn f() -> Int\n    @frozen x = 1\n").unwrap_err().code.as_str(), "E_PARSE");
    assert_eq!(parse("parallel\n    1\n").unwrap_err().code.as_str(), "E_PARSE");
}
