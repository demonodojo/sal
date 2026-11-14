use sal_compiler::fmt::format_program;
use sal_compiler::parser::parse;

#[test]
fn parse_hello() {
    let src = include_str!("../examples/hello.sal");
    let p = parse(src).expect("parse hello");
    assert!(p.items.iter().any(|i| matches!(i, sal_compiler::ast::Item::Fn(f) if f.name == "main")));
}

#[test]
fn fmt_module_const_idempotent() {
    for src in ["MAX = 1 + 2\n", "MAX: Int = 64\n", "@deprecated(\"old\")\nMAX = 1\n"] {
        let p1 = parse(src).expect("parse");
        let f1 = format_program(&p1);
        let p2 = parse(&f1).expect("re-parse");
        let f2 = format_program(&p2);
        assert_eq!(f1, f2, "fmt not idempotent:\n{f1}");
    }
}

#[test]
fn fmt_idempotent() {
    let src = include_str!("../examples/hello.sal");
    let p1 = parse(src).expect("parse");
    let f1 = format_program(&p1);
    let p2 = parse(&f1).expect("re-parse");
    let f2 = format_program(&p2);
    assert_eq!(f1, f2);
}

#[test]
fn fmt_arena_keep_idempotent() {
    let src = "\
@arena
fn handle(cache: &mut Dict[String, String]) -> Int ! alloc
    0
";
    let p1 = parse(src).expect("parse");
    let f1 = format_program(&p1);
    let p2 = parse(&f1).expect("re-parse");
    let f2 = format_program(&p2);
    assert_eq!(f1, f2);
    assert!(f1.contains("@arena"));
    assert!(f1.contains("&mut Dict[String, String]"));
}

#[test]
fn ast_json_roundtrip() {
    let src = include_str!("../examples/hello.sal");
    let p = parse(src).expect("parse");
    let j = serde_json::to_string(&p).unwrap();
    let p2: sal_compiler::ast::Program = serde_json::from_str(&j).unwrap();
    assert_eq!(p, p2);
}

#[test]
fn fmt_match_string_and_block_arm() {
    let src = "fn f(s: String) -> Int\n    match s\n        \"a\" => 1\n        r\"b\" => 2\n        _ =>\n            x = 3\n            x\n";
    let p = parse(src).expect("parse match");
    let f1 = format_program(&p);
    let p2 = parse(&f1).expect("re-parse match");
    let f2 = format_program(&p2);
    assert_eq!(f1, f2);
    assert!(f1.contains("r\"b\""));
}

#[test]
fn fmt_grouped_newlines_match_one_line() {
    let multi = "fn f() -> Int\n    (\n        1\n        + 2\n        + 3\n    )\n";
    let one = "fn f() -> Int\n    1 + 2 + 3\n";
    let pm = parse(multi).expect("parse multiline");
    let po = parse(one).expect("parse one line");
    let fm = format_program(&pm);
    let fo = format_program(&po);
    assert_eq!(fm, fo);
    let f2 = format_program(&parse(&fm).expect("reparse"));
    assert_eq!(fm, f2);
}

#[test]
fn fmt_lambda_arguments_and_function_types() {
    let src = "fn apply(f: fn(Int, Int) -> Int, a: Int) -> Int\n    a.apply f (x, y) => x + y\n";
    let p = parse(src).expect("parse function type and trailing lambda");
    let formatted = format_program(&p);
    let reparsed = parse(&formatted).expect("reparse formatted lambda");
    assert_eq!(formatted, format_program(&reparsed));
    assert!(formatted.contains("fn(Int, Int) -> Int"));
    assert!(formatted.contains("(x, y) => x + y"));
}

#[test]
fn fmt_protocol_provided_method_body() {
    let src = "protocol Value\n    method value(self) -> Int\n        7\n";
    let p1 = parse(src).expect("parse protocol body");
    let formatted = format_program(&p1);
    let p2 = parse(&formatted).expect("reparse protocol body");
    assert_eq!(formatted, format_program(&p2));
    let protocol = p1.items.iter().find_map(|item| match item {
        sal_compiler::ast::Item::Protocol(protocol) => Some(protocol),
        _ => None,
    }).expect("protocol item");
    assert!(protocol.methods[0].body.is_some());
}

#[test]
fn fmt_protocol_static_and_self() {
    let src = "protocol Model\n    static build(id: Int, name: String) -> Self ! alloc\n    static zero() -> Self\n        Self.build(0, \"\")\n    method id(&self) -> Int\n";
    let p1 = parse(src).expect("parse protocol statics");
    let formatted = format_program(&p1);
    assert_eq!(formatted, src);
    assert_eq!(formatted, format_program(&parse(&formatted).expect("reparse")));
    let protocol = p1.items.iter().find_map(|item| match item {
        sal_compiler::ast::Item::Protocol(protocol) => Some(protocol),
        _ => None,
    }).expect("protocol item");
    let statics: Vec<bool> = protocol.methods.iter().map(|m| m.is_static).collect();
    assert_eq!(statics, vec![true, true, false]);
    assert!(protocol.methods[1].body.is_some());
    assert!(protocol.methods[0].params.len() == 2);
}

#[test]
fn fmt_generic_protocol_and_claim() {
    let src = "protocol Value[T]\n    method map[U](&self, f: fn(T) -> U) -> U\n\nstruct Number impl Value[Int]\n    n: Int\n";
    let p = parse(src).expect("parse generic protocol");
    let formatted = format_program(&p);
    assert!(formatted.contains("protocol Value[T]"));
    assert!(formatted.contains("method map[U](&self, f: fn(T) -> U) -> U"));
    assert!(formatted.contains("struct Number impl Value[Int]"));
    assert_eq!(formatted, format_program(&parse(&formatted).expect("reparse")));
}

#[test]
fn fmt_impl_item() {
    let src = "impl List[T] Collection[T]\n";
    let p = parse(src).expect("parse impl item");
    let formatted = format_program(&p);
    assert_eq!(formatted, src);
    assert_eq!(formatted, format_program(&parse(&formatted).expect("reparse")));
}
