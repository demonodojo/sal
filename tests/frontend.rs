//! Frontend grammar coverage for the sal bootstrap parser/fmt.
//! No network; pure parse + format checks.

use sal_compiler::ast::{
    BinOp, Dim, Effect, Expr, Item, Pattern, Place, Stmt, StringPart, TensorElem, Type, TypeArg,
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
fn if_without_else_has_no_else_block() {
    match tail_of(
        "fn f() -> Int\n    if true\n        1\n",
    ) {
        Expr::If { else_block, .. } => assert!(else_block.is_none()),
        other => panic!("expected if, got {other:?}"),
    }
    match tail_of(
        "fn f() -> Int\n    if true\n        1\n    else\n        2\n",
    ) {
        Expr::If { else_block, .. } => assert!(else_block.is_some()),
        other => panic!("expected if with else, got {other:?}"),
    }
}

#[test]
fn if_else_binds_to_inner_if() {
    let src = r#"fn f() -> Int
    if true
        if false
            1
        else
            2
    0
"#;
    let p = assert_parse(src);
    let Item::Fn(f) = &p.items[0] else { panic!() };
    assert_eq!(f.body.stmts.len(), 1);
    let Stmt::Expr(Expr::If {
        then_block,
        else_block,
        ..
    }) = &f.body.stmts[0]
    else {
        panic!("expected outer if as statement");
    };
    assert!(else_block.is_none());
    let Some(Expr::If {
        else_block: inner_else,
        ..
    }) = then_block.tail.as_deref()
    else {
        panic!("expected inner if in then tail");
    };
    assert!(inner_else.is_some());
    assert!(matches!(f.body.tail.as_deref(), Some(Expr::Int { value: 0, .. })));
}

#[test]
fn elsif_chain_is_flat() {
    match tail_of(
        "fn f() -> Int\n    if 0\n        1\n    elsif 0\n        2\n    elsif 1\n        3\n    else\n        4\n",
    ) {
        Expr::If {
            elsifs,
            else_block,
            then_block,
            ..
        } => {
            assert_eq!(elsifs.len(), 2);
            assert!(matches!(
                then_block.tail.as_deref(),
                Some(Expr::Int { value: 1, .. })
            ));
            assert!(matches!(
                elsifs[0].body.tail.as_deref(),
                Some(Expr::Int { value: 2, .. })
            ));
            assert!(matches!(
                elsifs[1].body.tail.as_deref(),
                Some(Expr::Int { value: 3, .. })
            ));
            let Some(else_block) = else_block else {
                panic!("expected else");
            };
            assert!(matches!(
                else_block.tail.as_deref(),
                Some(Expr::Int { value: 4, .. })
            ));
        }
        other => panic!("expected if, got {other:?}"),
    }
}

#[test]
fn fmt_if_without_else_is_idempotent() {
    use sal_compiler::fmt::format_program;
    let src = "fn main() -> Int\n    if true\n        1\n    0\n";
    let p1 = parse(src).expect("parse");
    let f1 = format_program(&p1);
    assert!(!f1.contains("else"));
    let p2 = parse(&f1).expect("re-parse");
    let f2 = format_program(&p2);
    assert_eq!(f1, f2);
}

#[test]
fn fmt_elsif_is_idempotent() {
    use sal_compiler::fmt::format_program;
    let src = "fn f() -> Int\n    if 0\n        1\n    elsif 0\n        2\n    else\n        3\n";
    let p1 = parse(src).expect("parse");
    let f1 = format_program(&p1);
    assert!(f1.contains("    if 0\n"));
    assert!(f1.contains("    elsif 0\n"));
    assert!(f1.contains("    else\n"));
    let p2 = parse(&f1).expect("re-parse");
    let f2 = format_program(&p2);
    assert_eq!(f1, f2);
}

#[test]
fn dstring_desugars_to_strdup_call() {
    let dup = assert_parse(
        "\
fn main() -> String ! alloc
    d\"cadena\"
",
    );
    let plain = assert_parse(
        "\
fn main() -> String ! alloc
    strdup(\"cadena\")
",
    );
    let tail_dup = fn_tail_expr(&dup);
    let tail_plain = fn_tail_expr(&plain);
    assert!(same_strdup_string_call(tail_dup, tail_plain));
}

#[test]
fn dstring_interpolation_parts() {
    let src = "\
fn greet(name: String) -> String ! alloc
    d\"Hello, {name}\"
";
    let p = assert_parse(src);
    let Item::Fn(f) = &p.items[0] else { panic!() };
    let Some(Expr::Call { args, .. }) = f.body.tail.as_deref() else {
        panic!("expected strdup call");
    };
    let Some(Expr::String { value, parts, .. }) = args.first() else {
        panic!("expected string arg");
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
fn d_ident_not_dstring_without_quote() {
    let p = assert_parse(
        "\
fn main() -> Int
    let d = 1
    d
",
    );
    let Item::Fn(f) = &p.items[0] else { panic!() };
    assert!(matches!(
        &f.body.stmts[0],
        Stmt::Let { name, .. } if name == "d"
    ));
    assert!(matches!(
        f.body.tail.as_deref(),
        Some(Expr::Ident { name, .. }) if name == "d"
    ));
    let p2 = assert_parse(
        "\
fn main() -> Int
    d(1)
",
    );
    let Item::Fn(f2) = &p2.items[0] else { panic!() };
    let Some(Expr::Call { func, .. }) = f2.body.tail.as_deref() else {
        panic!("expected call");
    };
    assert!(matches!(func.as_ref(), Expr::Ident { name, .. } if name == "d"));
}

#[test]
fn string_plus_parses_as_left_assoc_add() {
    let src = "\
fn main() -> String ! alloc
    d\"a\" + d\"b\" + d\"c\"
";
    let p = assert_parse(src);
    let tail = fn_tail_expr(&p);
    let Expr::Binary {
        op: BinOp::Add,
        left,
        right,
        ..
    } = tail
    else {
        panic!("expected +");
    };
    assert!(matches!(right.as_ref(), Expr::Call { .. } | Expr::String { .. }));
    let Expr::Binary {
        op: BinOp::Add,
        left: inner_left,
        right: inner_right,
        ..
    } = left.as_ref()
    else {
        panic!("expected left-assoc +");
    };
    assert!(matches!(inner_left.as_ref(), Expr::Call { .. }));
    assert!(matches!(inner_right.as_ref(), Expr::Call { .. }));
}

#[test]
fn fmt_dstring_to_strdup() {
    assert_fmt_idempotent(
        "\
fn main() -> String ! alloc
    d\"cadena\"
",
    );
    let p = assert_parse(
        "\
fn main() -> String ! alloc
    d\"cadena\"
",
    );
    let f = format_program(&p);
    assert!(f.contains("strdup(\"cadena\")"), "fmt output: {f}");
}

fn fn_tail_expr(p: &sal_compiler::ast::Program) -> &Expr {
    let Item::Fn(f) = &p.items[0] else { panic!("expected fn") };
    f.body.tail.as_deref().expect("expected tail expr")
}

fn same_strdup_string_call(a: &Expr, b: &Expr) -> bool {
    let (Expr::Call { func: fa, type_args: ta, args: aa, .. }, Expr::Call {
        func: fb,
        type_args: tb,
        args: ab,
        ..
    }) = (a, b)
    else {
        return false;
    };
    matches!(fa.as_ref(), Expr::Ident { name, .. } if name == "strdup")
        && matches!(fb.as_ref(), Expr::Ident { name, .. } if name == "strdup")
        && ta.is_empty()
        && tb.is_empty()
        && aa.len() == 1
        && ab.len() == 1
        && matches!(
            (&aa[0], &ab[0]),
            (
                Expr::String {
                    value: va,
                    parts: pa,
                    ..
                },
                Expr::String {
                    value: vb,
                    parts: pb,
                    ..
                }
            ) if va == vb && pa == pb
        )
}

#[test]
fn tab_at_and_parallel_are_parse_errors() {
    assert_eq!(parse("fn f() -> Int\n\t1\n").unwrap_err().code.as_str(), "E_PARSE");
    assert_eq!(parse("fn f() -> Int\n    @frozen x = 1\n").unwrap_err().code.as_str(), "E_PARSE");
    assert_eq!(parse("parallel\n    1\n").unwrap_err().code.as_str(), "E_PARSE");
}

#[test]
fn paren_group_keeps_one_left_assoc_expr() {
    let src = "\
fn f() -> Int
    (
        1
        + 2
        + 3
    )
";
    match tail_of(src) {
        Expr::Binary { op: BinOp::Add, left, right, .. } => {
            assert!(matches!(right.as_ref(), Expr::Int { value: 3, .. }));
            match left.as_ref() {
                Expr::Binary { op: BinOp::Add, left, right, .. } => {
                    assert!(matches!(left.as_ref(), Expr::Int { value: 1, .. }));
                    assert!(matches!(right.as_ref(), Expr::Int { value: 2, .. }));
                }
                other => panic!("expected left-assoc add, got {other:?}"),
            }
        }
        other => panic!("expected add, got {other:?}"),
    }
    assert_fmt_idempotent(src);
}

#[test]
fn paren_group_allows_operator_at_end_of_line() {
    match tail_of("fn f() -> String ! alloc\n    (\n        \"a\" +\n        \"b\" +\n        \"c\"\n    )\n") {
        Expr::Binary { op: BinOp::Add, left, right, .. } => {
            assert!(matches!(right.as_ref(), Expr::String { value, .. } if value == "c"));
            assert!(matches!(
                left.as_ref(),
                Expr::Binary { op: BinOp::Add, .. }
            ));
        }
        other => panic!("expected string add, got {other:?}"),
    }
}

#[test]
fn newline_outside_parens_ends_the_statement() {
    let err = parse("fn f() -> Int\n    1\n    + 2\n").unwrap_err();
    assert_eq!(err.code.as_str(), "E_PARSE");
}

#[test]
fn call_and_brackets_may_break_across_lines() {
    match tail_of("fn f() -> Int\n    g(\n        1,\n        2\n    )\n") {
        Expr::Call { args, .. } => assert_eq!(args.len(), 2),
        other => panic!("expected call, got {other:?}"),
    }
    let p = assert_parse("fn f() -> Int\n    t = tensor[\n        [1.0, 2.0],\n        [3.0, 4.0]\n    ]\n    0\n");
    let Item::Fn(f) = &p.items[0] else { panic!("expected fn") };
    match &f.body.stmts[0] {
        Stmt::Assign { value, .. } => match value {
            Expr::TensorLit { rows, .. } => {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].len(), 2);
            }
            other => panic!("expected tensor, got {other:?}"),
        },
        other => panic!("expected assign, got {other:?}"),
    }
}
