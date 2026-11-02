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
        if p.file_name().is_some_and(|n| n == "target" || n == ".git") {
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
    s = d"a{{b}}"
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

fn borrowed(s: String borrow, t: String take) -> Int
    0

fn generic[T](x: T) -> T
    x

fn unit() -> Unit
    return

fn fields(p: Pair) -> Int
    p.a = p.b
    p.a + u.triple(1)
"#;

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
    copy_tree(&root.join("tests/lang/mods"), &work.join("mods"));
    let ir = |f: &Path| {
        Command::new(env!("CARGO_BIN_EXE_sal"))
            .args(["emit", "ir"])
            .arg(f)
            .current_dir(&work)
            .output()
            .unwrap()
    };
    let mut files: Vec<_> = std::fs::read_dir(root.join("tests/lang"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sal"))
        .collect();
    files.sort();
    let mut compared = 0;
    let mut failures = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap();
        let Ok(prog) = parse(&src) else { continue };
        let name = f.file_name().unwrap();
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
                name.to_string_lossy(),
                String::from_utf8_lossy(&b.stderr)
            ));
        }
    }
    assert!(compared >= 40, "only {compared} programs compared");
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
