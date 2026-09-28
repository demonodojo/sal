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
fn string_plus_appends_handle_via_str_from_int() {
    let src = r#"
fn main() -> Int ! alloc, io
    v = vec_new()
    _p = vec_push(v, d"Z")
    s = d"A" + d"B" + str_from_int(vec_get(v, 0))
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
fn string_plus_struct_field_and_fresh_temp_at_runtime() {
    let src = r#"
struct Msg
    text: String
    n: Int

fn main() -> Int ! alloc, io
    m = Msg(d"k", 7)
    s = m.text + "=" + int_to_str(m.n)
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
    assert!(
        art.ir_text.contains("sal_free"),
        "the int_to_str temp is freed after the append:\n{}",
        art.ir_text
    );
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "k=7");
}

#[test]
fn string_accumulator_loop_appends_without_copy() {
    // 5000 steps of `acc = acc + piece`: linear with in-place append.
    let src = r#"
fn fill(acc: String, i: Int, n: Int) -> String ! alloc
    if i >= n
        acc
    else
        acc = acc + "ab"
        fill(acc, i + 1, n)

fn main() -> Int ! alloc, io
    s = fill(d"", 0, 5000)
    _q = print_str(int_to_str(str_len(s)))
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
    let fill = art
        .ir_text
        .split("fn main:")
        .next()
        .unwrap_or(&art.ir_text)
        .to_string();
    assert!(
        fill.contains("sal_str_append") && !fill.contains("sal_str_concat"),
        "acc = acc + lit must append in place:\n{fill}"
    );
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "10000");
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
fn newtype_llvm_has_no_extra_storage() {
    let tag = r#"
struct Tag
    raw: Int

fn main() -> Int
    t = Tag(7)
    t.raw
"#;
    let plain = r#"
fn main() -> Int
    7
"#;
    let opts = CompileOptions {
        release: true,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: true,
    };
    let tagged = compile_source(tag, &opts).expect("tag");
    let bare = compile_source(plain, &opts).expect("int");
    let tagged_at = tagged.llvm.find("define i64 @main").expect("main");
    let bare_at = bare.llvm.find("define i64 @main").expect("main");
    let tagged_body = &tagged.llvm[tagged_at..];
    let bare_body = &bare.llvm[bare_at..];
    assert_eq!(tagged_body, bare_body, "newtype body:\n{tagged_body}");
    assert!(!tagged_body.contains("alloca"), "{tagged_body}");
    assert!(!tagged_body.contains("insertvalue"), "{tagged_body}");
    assert!(!tagged_body.contains("call ptr @malloc"), "{tagged_body}");
}

#[test]
fn list_string_and_dict_bool_kinds() {
    let src = r#"
fn main() -> Int ! alloc
    xs = list_new[String]()
    _ = dict_new[Int, Bool]()
    list_len(xs)
"#;
    let opts = CompileOptions {
        release: false,
        instrument: false,
        device: "cpu".into(),
        project_root: root(),
        skip_link: true,
    };
    let art = compile_source(src, &opts).expect("compile");
    assert!(
        art.llvm.contains("call ptr @sal_list_new_typed(i64 3)"),
        "{}",
        art.llvm
    );
    assert!(
        art.llvm.contains("call ptr @sal_dict_new(i64 0, i64 2)"),
        "{}",
        art.llvm
    );
}

#[test]
fn struct_field_assign_rebuilds_owner() {
    let src = r#"
struct Point
    x: Int
    y: Int

fn main() -> Int
    p = Point(3, 4)
    p.x = 10
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
    assert_eq!(out.status.code(), Some(14), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn struct_string_field_drops_clean() {
    let src = r#"
struct Msg
    text: String
    n: Int

fn main() -> Int ! alloc
    m = Msg(strdup("ab"), 1)
    str_len(m.text) + m.n
"#;
    let opts = CompileOptions {
        release: false,
        instrument: true,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "stderr: {err}");
    assert!(!err.contains("LEAK"), "{err}");
    assert!(!err.contains("DOUBLE_FREE"), "{err}");
}

#[test]
fn moved_string_newtype_drops_clean() {
    let src = r#"
struct Msg
    text: String

fn eat(m: Msg take) -> Int
    str_len(m.text)

fn main() -> Int ! alloc
    m = Msg(strdup("ab"))
    eat(m)
"#;
    let opts = CompileOptions {
        release: false,
        instrument: true,
        device: "cpu".into(),
        project_root: root(),
        skip_link: false,
    };
    let art = compile_source(src, &opts).expect("compile");
    let bin = art.binary.expect("binary");
    let out = Command::new(bin).output().expect("run");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {err}");
    assert!(!err.contains("LEAK"), "{err}");
    assert!(!err.contains("DOUBLE_FREE"), "{err}");
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
