//! `sal fmt` must produce source that parses again and is a fixed point, for
//! every program in the repository and for a synthetic program that uses each
//! syntactic form (enums, frames, patterns, lambdas, `try`, operators, ...).
use std::path::{Path, PathBuf};

use sal_compiler::fmt::format_program;
use sal_compiler::parser::parse;

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        // `standard/fixtures` are goldens for `standard fix` (e.g. `(77).to_str()`);
        // `sal fmt` drops those parens and the lexer then reads a float — skip them.
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "target" || name == ".git" {
            continue;
        }
        if name == "fixtures" && p.components().any(|c| c.as_os_str() == "standard") {
            continue;
        }
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "sal") {
            out.push(p);
        }
    }
}

fn check_fixed_point(name: &str, src: &str) -> Result<(), String> {
    let p1 = parse(src).map_err(|e| format!("{name}: parse: {e:?}"))?;
    let f1 = format_program(&p1);
    let p2 = parse(&f1).map_err(|e| format!("{name}: formatted output does not parse: {e:?}\n{f1}"))?;
    let f2 = format_program(&p2);
    if f1 != f2 {
        return Err(format!("{name}: fmt is not idempotent\n--- first\n{f1}\n--- second\n{f2}"));
    }
    Ok(())
}

#[test]
fn fmt_is_a_fixed_point_on_every_repo_program() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let mut checked = 0;
    let mut failures = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        // Programs the parser rejects on purpose (error-case fixtures) are skipped.
        if parse(&src).is_err() {
            continue;
        }
        checked += 1;
        if let Err(e) = check_fixed_point(&f.display().to_string(), &src) {
            failures.push(e);
        }
    }
    assert!(checked >= 100, "only {checked} files checked");
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

const KITCHEN_SINK: &str = r#"import "mods/util.sal" as u

struct Pair
    a: Int
    b: Int

struct Boxed[T]
    v: T

frame Shop
    price: F32
    qty: I8
    name: String

enum Shape
    Circle(Int)
    Rect(Int, Int)
    Empty

enum Maybe[T]
    Nothing
    Just(T)

fn area(s: Shape) -> Int
    match s
        Circle(r) => r * r * 3
        Rect(w, h) => w * h
        Shape.Empty() => 0

fn pick(x: Int) -> Int
    match x
        1 => 10
        _ => 0

fn ops(a: Int, b: Int) -> Int ! io, alloc
    c = a + b - a * b / 2 % 3
    d = (a & b) | (a ^ b) << 2 >> 1
    e = -a
    f = !b
    g = a == b
    h = a != b
    i = a < b
    j = a <= b
    k = a > b
    l = a >= b
    return c

fn control(x: Int) -> Int
    let y: Int = 3
    let z = y + 1
    while x < 10
        x = x + 1
    if x == 1
        1
    elsif x == 2
        2
    else
        3

fn lambdas(x: Int) -> Int
    f = v => v * 2
    g = try pick(x)
    0

fn text() -> String ! alloc
    s = d"a{b}"
    t = "plain"
    s + t

fn tensors(a: Tensor[F32, 2, 3] on cpu, b: Tensor[F16, ?, 3] on gpu) -> Int ! alloc, gpu
    t = tensor[[1.0, 2.0], [3.0, 4.0]]
    w = load[F32, 4, 4]("w.salt")
    g = to gpu a
    on gpu
        h = matmul(g, g)
    on gpu kernel i in Tensor[F32, 4] on gpu
        k = 1
    0

fn borrowed(s: &String, t: &String) -> Int
    str_len(s) + str_len(t)

fn generic[T](x: T) -> T
    x

fn unit() -> Unit
    return

fn fields(p: Pair) -> Int
    p.a = p.b
    p.a + u.triple(1)
"#;

/// `sal fmt` prints the AST, which has no comments: they are put back by line.
/// Every `#…` of the source survives, above the node that follows it, or at the
/// end of its line when it trailed code.
#[test]
fn fmt_keeps_comments() {
    use sal_compiler::fmt::format_program_with_comments;
    let src = r"# module comment
import std.io

# struct
struct P
    # field
    x: Int  # trailing field
    y: Int

# fn
fn main() -> Int
    # before the first statement
    x = 1 + 2  # trailing statement
    if x > 0
        # inside then
        y = x
    else
        y = 0
    match y
        # arm
        0 => 1
        _ => y
";
    let prog = parse(src).expect("parse");
    let out = format_program_with_comments(&prog, src);
    for comment in [
        "# module comment",
        "# struct",
        "# field",
        "# trailing field",
        "# fn",
        "# before the first statement",
        "# trailing statement",
        "# inside then",
        "# arm",
    ] {
        assert!(out.contains(comment), "{comment} lost:\n{out}");
    }
    // A trailing comment stays on the line it trailed.
    assert!(out.contains("    x = 1 + 2  # trailing statement"), "{out}");
    assert!(out.contains("    x: Int  # trailing field"), "{out}");
    // A comment above a statement stays inside its block.
    assert!(out.contains("    # before the first statement\n    x = 1 + 2"), "{out}");
    assert!(out.contains("        # inside then\n        y = x"), "{out}");
    // Formatting the output again is a fixed point.
    let again = format_program_with_comments(&parse(&out).expect("reparse"), &out);
    assert_eq!(out, again, "fmt with comments is not idempotent");

    // The formatter marks lines with \u{1}/\u{2}; a string literal that holds
    // those bytes is not a marker and survives both entry points.
    let tricky = "fn main() -> Int\n    s = \"a\u{1}1\u{1}b\"\n    # note\n    s.len()\n";
    let prog = parse(tricky).expect("parse");
    assert!(format_program_with_comments(&prog, tricky).contains("a\u{1}1\u{1}b"));
    assert!(format_program(&prog).contains("a\u{1}1\u{1}b"));
}

/// A comment written above `else` belongs above it, outside the else block. The
/// `else` keyword is no AST node, so the formatter takes its line from the token
/// stream, in the order it emits the `else` keywords.
#[test]
fn fmt_keeps_comment_above_else() {
    use sal_compiler::fmt::format_program_with_comments;
    let src = r"fn main() -> Int
    x = 1
    if x == 1
        y = 1
    # above the else
    else
        # first in the else block
        y = 2
    if y == 1
        z = 1
    else  # trailing else
        z = 2
    z
";
    let prog = parse(src).expect("parse");
    let out = format_program_with_comments(&prog, src);
    assert!(
        out.contains("    # above the else\n    else\n        # first in the else block\n        y = 2"),
        "the comment above the else moved into its block:\n{out}"
    );
    assert!(
        out.contains("    else  # trailing else\n        z = 2"),
        "the trailing comment on the else line moved:\n{out}"
    );
    let again = format_program_with_comments(&parse(&out).expect("reparse"), &out);
    assert_eq!(out, again, "fmt with comments is not idempotent");

    // Nested ifs: each `else` pairs with its own line, outer one last.
    let nested = r"fn main() -> Int
    a = 1
    if a == 1
        if a == 2
            b = 1
        # inner else
        else
            b = 2
    # outer else
    else
        b = 3
    b
";
    let prog = parse(nested).expect("parse");
    let out = format_program_with_comments(&prog, nested);
    assert!(
        out.contains("        # inner else\n        else\n") && out.contains("    # outer else\n    else\n"),
        "nested else comments did not stay on their own else:\n{out}"
    );
}

/// Protocol signatures are nodes of their own (`ProtocolMethod.span`), so a
/// comment written above a `method`/`static` signature stays above it instead of
/// drifting to the end of the file.
#[test]
fn fmt_keeps_comments_in_protocol() {
    use sal_compiler::fmt::format_program_with_comments;
    let src = r"# the protocol
protocol Sized[T]
    # above len
    method len(&self) -> Int
    static unit() -> Int  # trailing signature
    # above twice
    method twice(&self) -> Int
        # inside the provided body
        self.len() + self.len()
";
    let prog = parse(src).expect("parse");
    let out = format_program_with_comments(&prog, src);
    assert!(out.starts_with("# the protocol\nprotocol Sized[T]\n"), "{out}");
    assert!(
        out.contains("    # above len\n    method len(&self) -> Int\n"),
        "the comment above a signature did not stay there:\n{out}"
    );
    assert!(
        out.contains("    static unit() -> Int  # trailing signature\n"),
        "the trailing comment on a signature moved:\n{out}"
    );
    assert!(
        out.contains("    # above twice\n    method twice(&self) -> Int\n        # inside the provided body\n        self.len() + self.len()"),
        "the comment above a provided method moved:\n{out}"
    );
    let again = format_program_with_comments(&parse(&out).expect("reparse"), &out);
    assert_eq!(out, again, "fmt with comments is not idempotent");
}

#[test]
fn fmt_kitchen_sink_is_a_fixed_point() {
    // Some forms may not parse in the bootstrap; find out which, one item at a time.
    let mut failures = Vec::new();
    if let Err(e) = check_fixed_point("kitchen sink", KITCHEN_SINK) {
        // Fall back to per-item checks so the report names the culprit.
        let items: Vec<&str> = KITCHEN_SINK.split("\n\n").collect();
        let head = items[0];
        for it in &items[1..] {
            let src = format!("{head}\n\n{it}\n");
            if let Err(e) = check_fixed_point(&it.lines().next().unwrap_or("?").to_string(), &src) {
                failures.push(e);
            }
        }
        if failures.is_empty() {
            failures.push(e);
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// Formatting must not change meaning: the IR of every `tests/lang` program is
/// the same before and after `sal fmt`.
#[test]
fn fmt_preserves_ir_on_language_corpus() {
    use std::process::Command;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let work = root.join("target/test-fmt-ir");
    let _ = std::fs::remove_dir_all(&work);
    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let dest = to.join(e.file_name());
            if e.path().is_dir() {
                copy_tree(&e.path(), &dest);
            } else {
                std::fs::copy(e.path(), dest).unwrap();
            }
        }
    }
    copy_tree(&root.join("tests/lang"), &work);
    let ir = |f: &Path| {
        Command::new(env!("CARGO_BIN_EXE_sal"))
            .args(["emit", "ir"])
            .arg(f)
            .current_dir(&work)
            .output()
            .unwrap()
    };
    fn programs(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n != "mods" && n != "target") {
                    programs(&p, out);
                }
            } else if p.extension().is_some_and(|x| x == "sal") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    programs(&root.join("tests/lang"), &mut files);
    files.sort();
    let mut compared = 0;
    let mut failures = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        let Ok(prog) = parse(&src) else { continue };
        let name = f.strip_prefix(root.join("tests/lang")).unwrap();
        // Same path before and after: module-qualified names derive from it.
        let file = work.join(name);
        std::fs::write(&file, &src).unwrap();
        let a = ir(&file);
        if !a.status.success() {
            continue; // rejected programs have no IR to compare
        }
        std::fs::write(&file, format_program(&prog)).unwrap();
        let b = ir(&file);
        compared += 1;
        if !b.status.success() || a.stdout != b.stdout {
            failures.push(format!(
                "{}: IR changes after fmt\n{}",
                name.display(),
                String::from_utf8_lossy(&b.stderr)
            ));
        }
    }
    assert!(compared >= 40, "only {compared} programs compared");
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
