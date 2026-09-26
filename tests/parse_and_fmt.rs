use sal_compiler::fmt::format_program;
use sal_compiler::parser::parse;

#[test]
fn parse_hello() {
    let src = include_str!("../examples/hello.sal");
    let p = parse(src).expect("parse hello");
    assert!(p.items.iter().any(|i| matches!(i, sal_compiler::ast::Item::Fn(f) if f.name == "main")));
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
fn ast_json_roundtrip() {
    let src = include_str!("../examples/hello.sal");
    let p = parse(src).expect("parse");
    let j = serde_json::to_string(&p).unwrap();
    let p2: sal_compiler::ast::Program = serde_json::from_str(&j).unwrap();
    assert_eq!(p, p2);
}
