use std::path::PathBuf;
use std::process::Command;

use sal_compiler::compile::compile_file;
use sal_compiler::compile_source;
use sal_compiler::CompileOptions;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn hello_runs_42() {
    let file = root().join("examples/hello.sal");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_file(&file, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(42));
}

#[test]
fn elsif_chain_runs() {
    let src = r#"
fn main() -> Int
    if 0
        1
    elsif 1
        7
    else
        3
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(7), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn incremental_cache_second_build() {
    let file = root().join("examples/hello.sal");
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: true,
    };
    let a = compile_file(&file, &opts).expect("first");
    let b = compile_file(&file, &opts).expect("second");
    assert!(b.cache_hit || a.cache_hit);
}

#[test]
fn string_plus_appends_nonstr_pointer() {
    let src = r#"
fn main() -> Int ! alloc, io
    v = vec_new()
    _p = vec_push(v, d"Z")
    s = d"A" + d"B" + vec_get(v, 0)
    _q = print_str(s)
    0
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ABZ");
}

#[test]
fn string_header_concat_slice_eq_and_literal_immutable() {
    let src = r#"
fn main() -> Int ! alloc
    lit = "ab"
    n0 = str_len(lit)
    heap = strdup("ab")
    chain = "ab" + "c" + "d"
    if str_len(chain) != 4
        10
    else
        if n0 != 2
            11
        else
            if str_len(lit) != 2
                12
            else
                _tail = str_append(heap, "z")
                if str_len(lit) != 2
                    13
                else
                    if str_eq(str_slice(chain, 0, 2), lit) == 1
                        0
                    else
                        14
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn struct_point_fields_sum_at_runtime() {
    let src = r#"
struct Point
    x: Int
    y: Int

fn main() -> Int
    p = Point(3, 4)
    p.x + p.y
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(7), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn option_some_match_returns_payload() {
    let src = r#"
enum Option[T]
    None
    Some(T)

fn main() -> Int
    o = Some(42)
    match o
        Some(x) => x
        None => 0
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(42), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn transparent_newtype_same_as_inner() {
    let src = r#"
struct Tag
    raw: Int

fn main() -> Int
    t = Tag(99)
    t.raw
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(99), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn assign_inside_if_is_visible_after() {
    let src = r#"
fn main() -> Int
    acc = 1
    if 1 == 1
        acc = 2
    else
        acc = 3
    acc
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn while_assignment_feeds_next_iteration() {
    let src = r#"
fn main() -> Int
    n = 3
    i = 0
    acc = 0
    while i < n
        acc = acc + i
        i = i + 1
    acc
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn while_sees_assignment_inside_if() {
    let src = r#"
fn main() -> Int
    n = 3
    i = 0
    acc = 0
    while i < n
        if 1 == 1
            acc = acc + i
        else
            acc = acc
        i = i + 1
    acc
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn grouped_newlines_run_as_one_expression() {
    let src = r#"
fn id(x: Int) -> Int
    x

fn main() -> Int ! alloc
    n = (
        1
        + 2

        + 3
    )
    s = (
        "ab" +
        "cd" +
        "ef"
    )
    id(
        n
    ) + str_len(s)
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile grouped newlines");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(
        out.status.code(),
        Some(12),
        "1+2+3 and len(\"abcdef\") must sum to 12; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
