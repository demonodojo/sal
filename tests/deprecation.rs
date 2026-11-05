//! `W_DEPRECATED`: `@deprecated("…")` on fn, method, struct and enum. A use warns and the
//! compilation goes on; the attribute is parsed, printed back by `sal fmt`, and refused elsewhere.

use std::fs;
use std::path::{Path, PathBuf};

use sal_compiler::ast::Program;
use sal_compiler::deprecation::{check_deprecations, Warning, W_DEPRECATED};
use sal_compiler::fmt::format_program;
use sal_compiler::modules::resolve_module_graph;
use sal_compiler::parser::parse;

fn warnings_of(src: &str) -> Vec<Warning> {
    let prog = parse(src).expect("parse");
    check_deprecations(&[(PathBuf::from("main.sal"), &prog)])
}

#[test]
fn call_of_deprecated_fn_warns_with_the_message() {
    let src = "@deprecated(\"use new_f\")\nfn old_f() -> Int\n    1\n\nfn new_f() -> Int\n    2\n\nfn main() -> Int\n    old_f() + new_f()\n";
    let ws = warnings_of(src);
    assert_eq!(ws.len(), 1, "{ws:?}");
    assert_eq!(ws[0].code, W_DEPRECATED);
    assert_eq!(ws[0].message, "`old_f` is deprecated: use new_f");
    assert_eq!(ws[0].span.line, 9);
}

#[test]
fn unused_deprecated_item_does_not_warn() {
    let src = "@deprecated(\"gone\")\nfn old_f() -> Int\n    1\n\nfn main() -> Int\n    2\n";
    assert!(warnings_of(src).is_empty());
}

#[test]
fn deprecated_struct_and_enum_warn_where_they_are_named() {
    let src = "@deprecated(\"use Pt2\")\nstruct Pt\n    x: Int\n\n@deprecated(\"use E2\")\nenum E\n    A\n\nfn f(p: Pt) -> Int\n    p.x\n\nfn g(e: E) -> Int\n    0\n\nfn main() -> Int\n    0\n";
    let msgs: Vec<String> = warnings_of(src).into_iter().map(|w| w.message).collect();
    assert_eq!(msgs, vec!["`Pt` is deprecated: use Pt2", "`E` is deprecated: use E2"]);
}

#[test]
fn deprecated_method_warns_at_the_resolved_call() {
    let src = "struct P\n    x: Int\n\n@deprecated(\"use value\")\nmethod P.old() -> Int\n    self.x\n\nmethod P.value() -> Int\n    self.x\n\nfn main() -> Int\n    p = P(3)\n    p.old() + p.value()\n";
    let mut prog = parse(src).expect("parse");
    sal_compiler::methods::prepare_program(&mut prog).expect("methods");
    let ws = check_deprecations(&[(PathBuf::from("main.sal"), &prog)]);
    assert_eq!(ws.len(), 1, "{ws:?}");
    assert_eq!(ws[0].message, "`P__old` is deprecated: use value");
}

#[test]
fn attribute_needs_a_string_message() {
    let err = parse("@deprecated\nfn f() -> Int\n    1\n").expect_err("no message");
    assert_eq!(err.code.as_str(), "E_PARSE");
}

#[test]
fn attribute_is_refused_on_let_and_other_items() {
    assert!(parse("fn main() -> Int\n    @deprecated(\"x\")\n    1\n").is_err());
    assert!(parse("@deprecated(\"x\")\nframe F\n    a: Int\n").is_err());
}

#[test]
fn fmt_keeps_the_attribute() {
    let src = "@deprecated(\"use \\\"b\\\"\")\nfn a() -> Int\n    1\n";
    let printed = format_program(&parse(src).expect("parse"));
    assert!(printed.starts_with("@deprecated(\"use \\\"b\\\"\")\nfn a()"), "{printed}");
    let again = format_program(&parse(&printed).expect("reparse"));
    assert_eq!(printed, again);
}

fn temp_project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sal-deprecation-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (file, body) in files {
        fs::write(dir.join(file), body).unwrap();
    }
    dir
}

fn module_warnings(root: &Path, dir: &Path) -> Vec<Warning> {
    let graph = resolve_module_graph(root, dir).expect("graph");
    let modules: Vec<(PathBuf, &Program)> = graph.order.iter().map(|m| (m.path.clone(), &m.program)).collect();
    check_deprecations(&modules)
}

#[test]
fn use_from_another_module_warns_through_alias_and_plain_import() {
    let lib = "@deprecated(\"use two\")\nfn one() -> Int\n    1\n\nfn two() -> Int\n    2\n";
    let main = "import \"lib.sal\" as l\nimport \"util.sal\"\n\nfn main() -> Int\n    l.one() + l.two() + helper()\n";
    let util = "@deprecated(\"use fine\")\nfn helper() -> Int\n    5\n\nfn fine() -> Int\n    1\n";
    let dir = temp_project("cross", &[("lib.sal", lib), ("util.sal", util), ("main.sal", main)]);
    let ws = module_warnings(&dir.join("main.sal"), &dir);
    let msgs: Vec<String> = ws.iter().map(|w| w.message.clone()).collect();
    assert!(msgs.contains(&"`one` is deprecated: use two".to_string()), "{msgs:?}");
    assert!(msgs.contains(&"`helper` is deprecated: use fine".to_string()), "{msgs:?}");
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert!(ws.iter().any(|w| w.path.ends_with("main.sal")));
    let _ = fs::remove_dir_all(&dir);
}
